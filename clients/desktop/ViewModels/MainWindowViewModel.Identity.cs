using System;
using System.IO;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using uniffi.aura_core;

namespace Aura.Desktop.ViewModels;

/// <summary>Settings → Identity: the key card, encrypted backup/restore, and Leave server.</summary>
public partial class MainWindowViewModel
{
    public const int MinPassphraseLength = 8;

    [ObservableProperty] private string _backupPassphrase = "";
    [ObservableProperty] private string _backupPassphraseRepeat = "";
    [ObservableProperty] private bool _isIdentityBusy;
    [ObservableProperty] private string _identityStatus = "";
    [ObservableProperty] private string _identityError = "";

    /// <summary>The initial on the identity card.</summary>
    public string IdentityInitial => OwnInitial;

    /// <summary>Full grouped fingerprint, mono in accent on the card.</summary>
    public string IdentityFingerprintFull
    {
        get
        {
            var key = _identity?.IdentityPublicKey;
            if (key == null || key.Length == 0) return "No identity yet — one is created when you first connect.";
            try { return AuraCoreMethods.IdentityFingerprint(key); }
            catch { return ""; }
        }
    }

    public bool HasIdentity => _identity != null;
    public bool NeverBackedUp => _settings.IdentityBackedUpAt == null;

    public string LastBackupLabel => _settings.IdentityBackedUpAt is { } at
        ? $"Last backup {at.ToLocalTime():d MMM yyyy}"
        : "";

    /// <summary>Why "Save encrypted backup…" can't run yet; empty when it can.</summary>
    public string PassphraseProblem =>
        BackupPassphrase.Length == 0 ? ""
        : BackupPassphrase.Length < MinPassphraseLength ? $"Use at least {MinPassphraseLength} characters."
        : BackupPassphraseRepeat.Length > 0 && BackupPassphraseRepeat != BackupPassphrase ? "The passphrases don't match."
        : "";

    public bool CanSaveBackup =>
        _identity != null && !IsIdentityBusy &&
        BackupPassphrase.Length >= MinPassphraseLength && BackupPassphraseRepeat == BackupPassphrase;

    public bool CanRestoreBackup => !IsIdentityBusy && BackupPassphrase.Length > 0;

    partial void OnBackupPassphraseChanged(string value) => NotifyPassphrase();
    partial void OnBackupPassphraseRepeatChanged(string value) => NotifyPassphrase();
    partial void OnIsIdentityBusyChanged(bool value) => NotifyPassphrase();

    private void NotifyPassphrase()
    {
        OnPropertyChanged(nameof(PassphraseProblem));
        OnPropertyChanged(nameof(CanSaveBackup));
        OnPropertyChanged(nameof(CanRestoreBackup));
    }

    private void NotifyIdentityCard()
    {
        OnPropertyChanged(nameof(IdentityInitial));
        OnPropertyChanged(nameof(IdentityFingerprintFull));
        OnPropertyChanged(nameof(HasIdentity));
        OnPropertyChanged(nameof(NeverBackedUp));
        OnPropertyChanged(nameof(LastBackupLabel));
        OnPropertyChanged(nameof(FingerprintDisplay));
        NotifyPassphrase();
    }

    /// <summary>Set by the view: pick where to save; returns a writable stream or null if cancelled.</summary>
    public Func<Task<Stream?>>? SaveBackupPicker { get; set; }

    /// <summary>Set by the view: pick a backup to open; returns its bytes or null if cancelled.</summary>
    public Func<Task<byte[]?>>? OpenBackupPicker { get; set; }

    [RelayCommand]
    private async Task SaveBackupAsync()
    {
        if (!CanSaveBackup || _identity == null || SaveBackupPicker == null) return;
        IdentityError = "";
        IdentityStatus = "";

        var stream = await SaveBackupPicker();
        if (stream == null) return;

        IsIdentityBusy = true;
        IdentityStatus = "Encrypting…";
        var identity = _identity;
        var passphrase = BackupPassphrase;
        try
        {
            // scrypt at N = 2^17: seconds, not milliseconds. Off the UI thread.
            var backup = await Task.Run(() => IdentityBackup.Encrypt(identity, passphrase));
            await using (stream)
            {
                await stream.WriteAsync(backup);
            }

            _settings.IdentityBackedUpAt = DateTimeOffset.UtcNow;
            SaveSettings();
            BackupPassphrase = "";
            BackupPassphraseRepeat = "";
            IdentityStatus = "Backup saved. Keep the passphrase somewhere other than the file.";
        }
        catch (Exception ex)
        {
            IdentityStatus = "";
            IdentityError = IdentityBackup.ErrorMessage(ex);
        }
        finally
        {
            IsIdentityBusy = false;
            NotifyIdentityCard();
        }
    }

    // ── Restore: open → decrypt → confirm → replace ──

    private RestoredIdentity? _restoreCandidate;

    [ObservableProperty] private bool _showRestoreConfirm;
    [ObservableProperty] private string _restoreCurrentLabel = "";
    [ObservableProperty] private string _restoreBackupLabel = "";

    [RelayCommand]
    private async Task RestoreBackupAsync()
    {
        if (!CanRestoreBackup || OpenBackupPicker == null) return;
        IdentityError = "";
        IdentityStatus = "";

        var bytes = await OpenBackupPicker();
        if (bytes == null) return;

        IsIdentityBusy = true;
        IdentityStatus = "Decrypting…";
        var passphrase = BackupPassphrase;
        try
        {
            var contents = await Task.Run(() => IdentityBackup.Decrypt(bytes, passphrase));
            _restoreCandidate = contents;

            RestoreCurrentLabel = _identity == null
                ? "This device: no identity"
                : $"This device: {_identity.DisplayName} · {IdentityBackup.ShortFingerprint(_identity.IdentityPublicKey)}";
            RestoreBackupLabel = $"Backup: {contents.DisplayName} · {contents.ShortFingerprint}";
            IdentityStatus = "";
            ShowRestoreConfirm = true;
        }
        catch (Exception ex)
        {
            IdentityStatus = "";
            IdentityError = IdentityBackup.ErrorMessage(ex);
        }
        finally
        {
            IsIdentityBusy = false;
        }
    }

    [RelayCommand]
    private void CancelRestore()
    {
        if (_restoreCandidate != null) Array.Clear(_restoreCandidate.Secret);
        _restoreCandidate = null;
        ShowRestoreConfirm = false;
    }

    /// <summary>
    /// Replace the identity file with the backup. The current file is kept,
    /// renamed alongside, so a mistaken restore can be undone by hand.
    /// </summary>
    [RelayCommand]
    private async Task ConfirmRestoreAsync()
    {
        var contents = _restoreCandidate;
        ShowRestoreConfirm = false;
        if (contents == null) return;

        try
        {
            // The live session belongs to the old key.
            if (IsConnected)
            {
                CancelReconnectInternal();
                await DisconnectInternalAsync();
                ConnectionStatus = "Disconnected";
            }

            var path = UserIdentity.GetIdentityFilePath();
            if (File.Exists(path))
            {
                var kept = $"{path}.replaced-{DateTime.Now:yyyyMMdd-HHmmss}";
                File.Move(path, kept);
            }

            var restored = UserIdentity.FromSecret(contents.Secret, contents.DisplayName);
            restored.Save(path);

            _identity?.Dispose();
            _identity = restored;
            DisplayName = restored.DisplayName;
            PublicKeyDisplay = restored.PublicKeyHex[..16] + "...";
            BackupPassphrase = "";
            BackupPassphraseRepeat = "";
            IdentityStatus = "Identity restored. Connect to use it.";
        }
        catch (Exception ex)
        {
            IdentityError = $"Couldn't restore: {ex.Message}";
        }
        finally
        {
            Array.Clear(contents.Secret);
            _restoreCandidate = null;
            NotifyIdentityCard();
        }
    }

    // ── Leave server ──

    /// <summary>Set while a delete-account request is in flight; suppresses auto-reconnect.</summary>
    private bool _leavingServer;

    [ObservableProperty] private bool _showLeaveConfirm;
    [ObservableProperty] private bool _isLeaving;

    public string LeaveConfirmTitle => $"Delete your account on {_sessionHost}?";
    public string LeaveConfirmBody =>
        $"The server releases the name \"{DisplayName}\" and removes your profile. Your key stays on this device, and you can join again later as someone new.";

    [RelayCommand]
    private void AskLeaveServer()
    {
        if (!IsAuthenticated) return;
        OnPropertyChanged(nameof(LeaveConfirmTitle));
        OnPropertyChanged(nameof(LeaveConfirmBody));
        ShowSettings = false;
        ShowLeaveConfirm = true;
    }

    [RelayCommand]
    private void CancelLeaveServer() => ShowLeaveConfirm = false;

    [RelayCommand]
    private async Task ConfirmLeaveServerAsync()
    {
        if (_client == null) return;
        IsLeaving = true;
        _leavingServer = true;
        try
        {
            await _client.DeleteOwnAccountAsync();
            // The outcome arrives as the 0x44 AdminResponse (HandleLeaveResult).
        }
        catch (Exception ex)
        {
            _leavingServer = false;
            IsLeaving = false;
            ShowLeaveConfirm = false;
            ShowBanner($"Couldn't leave {_sessionHost}: {ex.Message}", isError: true);
        }
    }

    /// <summary>The server's answer to "delete my account".</summary>
    private async Task HandleLeaveResultAsync(bool ok, string error)
    {
        ShowLeaveConfirm = false;
        IsLeaving = false;
        if (!ok)
        {
            _leavingServer = false;
            ShowBanner($"The server didn't delete your account: {error}", isError: true);
            return;
        }
        await FinishLeaveAsync();
    }

    /// <summary>
    /// Account gone: disconnect without reconnecting, and forget the saved
    /// server (the certificate pin stays — the server is still who it was).
    /// </summary>
    private async Task FinishLeaveAsync()
    {
        var host = _sessionHost;
        CancelReconnectInternal();
        _rejoinChannelId = null;
        await DisconnectInternalAsync();
        _leavingServer = false;
        ConnectionStatus = "Disconnected";

        _settings.LastServerHost = "";
        _settings.LastServerPort = AuraInviteLink.DefaultPort;
        SaveSettings();
        ServerPassword = "";
        _sessionPassword = "";

        ShowBanner($"Your account on {host} was deleted.", isError: false);
    }
}

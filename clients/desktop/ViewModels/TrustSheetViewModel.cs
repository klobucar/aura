using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using uniffi.aura_core;

namespace Aura.Desktop.ViewModels;

/// <summary>One row of the sheet's MEMBERS list.</summary>
public partial class TrustMemberViewModel : ObservableObject
{
    public string Uuid { get; init; } = "";
    public byte[] PublicKey { get; init; } = Array.Empty<byte>();
    public bool IsSelf { get; init; }

    [ObservableProperty] private string _name = "";
    [ObservableProperty] private TrustState _trust = TrustState.Tofu;
    [ObservableProperty] private bool _isSelected;

    /// <summary>`3f8a…5503`, the short identity hint beside the name.</summary>
    [ObservableProperty] private string _shortFingerprint = "";

    /// <summary>
    /// The trailing label. The handoff allows exactly three readings plus a
    /// block, and each is phrased to say what the user can act on rather than
    /// what the crypto did.
    /// </summary>
    public string StateLabel => Trust switch
    {
        TrustState.Verified => $"verified · {ShortFingerprint}",
        TrustState.KeyChanged => "key changed",
        TrustState.Blocked => "blocked",
        _ => "TOFU · not compared",
    };

    public bool IsVerified => Trust == TrustState.Verified;
    public bool IsKeyChanged => Trust == TrustState.KeyChanged;
    public bool IsBlocked => Trust == TrustState.Blocked;

    partial void OnTrustChanged(TrustState value)
    {
        OnPropertyChanged(nameof(StateLabel));
        OnPropertyChanged(nameof(IsVerified));
        OnPropertyChanged(nameof(IsKeyChanged));
        OnPropertyChanged(nameof(IsBlocked));
    }

    partial void OnShortFingerprintChanged(string value) => OnPropertyChanged(nameof(StateLabel));
}

/// <summary>
/// The trust sheet (§7): a glass sheet over the channel that makes E2EE
/// legible. Header, an optional key-changed banner, six safety words to read
/// out of band, both full fingerprints, verify/block, and the member list.
///
/// Every key here comes from <c>MlsWrapper.GroupMembers</c> — the authenticated
/// ratchet tree — never from the server's user list, which carries no key
/// material precisely so it cannot be used to assert identity. Display names
/// are only ever labels for a key, never the thing being trusted.
/// </summary>
public partial class TrustSheetViewModel : ObservableObject
{
    private readonly TrustStore _trust;
    private readonly Func<string, string?> _nameForUuid;

    private byte[] _localKey = Array.Empty<byte>();
    private string _localUuid = "";

    public TrustSheetViewModel(TrustStore trust, Func<string, string?> nameForUuid)
    {
        _trust = trust;
        _nameForUuid = nameForUuid;
    }

    [ObservableProperty] private bool _isOpen;
    [ObservableProperty] private string _channelName = "";

    /// <summary>`MLS · X25519 · AES-128-GCM · Ed25519 · epoch 14`.</summary>
    [ObservableProperty] private string _securityLine = "";

    public ObservableCollection<TrustMemberViewModel> Members { get; } = new();

    [ObservableProperty] private TrustMemberViewModel? _selected;

    /// <summary>Six BIP-39 words, laid out three per row.</summary>
    public ObservableCollection<string> SafetyWords { get; } = new();

    [ObservableProperty] private string _localFingerprint = "";
    [ObservableProperty] private string _remoteFingerprint = "";

    /// <summary>`SAFETY WORDS — sol ↔ you`.</summary>
    [ObservableProperty] private string _pairLabel = "";

    /// <summary>
    /// Deriving costs an scrypt run (~100ms), so the words fade in rather than
    /// blocking the sheet. Empty state is a chip, never a spinner.
    /// </summary>
    [ObservableProperty] private bool _isDeriving;

    [ObservableProperty] private string _error = "";

    public bool HasError => !string.IsNullOrEmpty(Error);
    partial void OnErrorChanged(string value) => OnPropertyChanged(nameof(HasError));

    /// <summary>The amber banner, shown only when the selected member's key moved.</summary>
    public bool ShowKeyChangedBanner => Selected?.IsKeyChanged == true;

    public string KeyChangedTitle => $"{Selected?.Name ?? "This user"}'s identity key changed";

    public string KeyChangedBody =>
        "Same display name, new Ed25519 key. Could be a reinstall — or a name "
        + "takeover. Compare the words below out of band before you speak.";

    public bool CanAct => Selected is { IsSelf: false } && !IsDeriving && SafetyWords.Count > 0;

    /// <summary>
    /// Open the sheet for a channel, rebuilding the member list from the MLS
    /// group. <paramref name="focusUuid"/> preselects a member — the rail's
    /// "verify key" row and the roster both arrive here with someone in mind.
    /// </summary>
    internal async Task OpenAsync(MlsWrapper mls, string channelId, string channelName, string? focusUuid = null)
    {
        ChannelName = channelName;
        Error = "";
        IsOpen = true;

        List<MlsGroupMemberRecord> members;
        ulong epoch;
        try
        {
            members = mls.GroupMembers(channelId, isVoice: true).ToList();
            epoch = mls.CurrentEpoch(channelId, isVoice: true);
        }
        catch (Exception ex)
        {
            // Not in the group yet, or no group at all. Say so plainly rather
            // than showing an empty sheet that looks like "nobody to verify".
            Error = $"Can't read the group's keys: {ex.Message}";
            Members.Clear();
            ClearDerived();
            return;
        }

        SecurityLine = $"MLS · X25519 · AES-128-GCM · Ed25519 · epoch {epoch}";

        var self = members.FirstOrDefault(m => m.isSelf);
        _localKey = self?.signatureKey ?? Array.Empty<byte>();
        _localUuid = self?.identity ?? "";

        Members.Clear();
        foreach (var m in members.OrderBy(m => m.isSelf).ThenBy(m => DisplayName(m.identity)))
        {
            // Record every key we see before judging it — that is what makes a
            // later disagreement detectable at all.
            _trust.Observe(m.identity, m.signatureKey);

            Members.Add(new TrustMemberViewModel
            {
                Uuid = m.identity,
                PublicKey = m.signatureKey,
                IsSelf = m.isSelf,
                Name = DisplayName(m.identity),
                Trust = _trust.Evaluate(m.identity, m.signatureKey),
                ShortFingerprint = ShortFingerprintOf(m.signatureKey),
            });
        }

        var focus = (focusUuid != null ? Members.FirstOrDefault(m => m.Uuid == focusUuid) : null)
                    ?? Members.FirstOrDefault(m => m.IsKeyChanged)
                    ?? Members.FirstOrDefault(m => !m.IsSelf);

        await SelectAsync(focus);
    }

    /// <summary>Focus a member and derive the words for them.</summary>
    public async Task SelectAsync(TrustMemberViewModel? member)
    {
        foreach (var m in Members) m.IsSelected = ReferenceEquals(m, member);

        Selected = member;
        OnPropertyChanged(nameof(ShowKeyChangedBanner));
        OnPropertyChanged(nameof(KeyChangedTitle));

        ClearDerived();

        if (member == null || member.IsSelf || _localKey.Length == 0) return;

        IsDeriving = true;
        OnPropertyChanged(nameof(CanAct));

        var remoteKey = member.PublicKey;
        var remoteUuid = member.Uuid;
        var localKey = _localKey;
        var localUuid = _localUuid;

        try
        {
            // scrypt at N=16384 — off the UI thread, or the sheet visibly stalls.
            var derived = await Task.Run(() => AuraCoreMethods.DerivePairwiseVerification(
                localKey, localUuid, remoteKey, remoteUuid));

            // The user may have moved on while this was deriving; a stale result
            // would show one member's words under another's name.
            if (!ReferenceEquals(Selected, member)) return;

            SafetyWords.Clear();
            foreach (var w in derived.safetyWords) SafetyWords.Add(w);

            LocalFingerprint = derived.localFingerprint;
            RemoteFingerprint = derived.remoteFingerprint;
            PairLabel = $"SAFETY WORDS — {member.Name} ↔ you";
        }
        catch (Exception ex)
        {
            if (ReferenceEquals(Selected, member)) Error = $"Couldn't derive safety words: {ex.Message}";
        }
        finally
        {
            if (ReferenceEquals(Selected, member)) IsDeriving = false;
            OnPropertyChanged(nameof(CanAct));
        }
    }

    [RelayCommand]
    private async Task Select(TrustMemberViewModel? member) => await SelectAsync(member);

    /// <summary>"They match — mark verified".</summary>
    [RelayCommand]
    private void MarkVerified()
    {
        if (Selected is not { IsSelf: false } m) return;

        _trust.MarkVerified(m.Uuid, m.PublicKey);
        m.Trust = _trust.Evaluate(m.Uuid, m.PublicKey);
        OnPropertyChanged(nameof(ShowKeyChangedBanner));
    }

    /// <summary>Block — the negative outcome of the same comparison.</summary>
    [RelayCommand]
    private void Block()
    {
        if (Selected is not { IsSelf: false } m) return;

        _trust.Block(m.PublicKey);
        m.Trust = _trust.Evaluate(m.Uuid, m.PublicKey);
        OnPropertyChanged(nameof(ShowKeyChangedBanner));
    }

    [RelayCommand]
    private void Close()
    {
        IsOpen = false;
        ClearDerived();
        Members.Clear();
        Selected = null;
    }

    private void ClearDerived()
    {
        SafetyWords.Clear();
        LocalFingerprint = "";
        RemoteFingerprint = "";
        PairLabel = "";
        IsDeriving = false;
        OnPropertyChanged(nameof(CanAct));
    }

    private string DisplayName(string uuid)
    {
        var name = _nameForUuid(uuid);
        if (!string.IsNullOrWhiteSpace(name)) return name;

        // A member in the group we have no roster entry for. Showing the raw
        // uuid beats inventing a name for someone we cannot identify.
        return uuid.Length > 8 ? uuid[..8] : uuid;
    }

    private static string ShortFingerprintOf(byte[] key)
    {
        try { return AuraCoreMethods.IdentityFingerprintShort(key); }
        catch { return ""; }
    }
}

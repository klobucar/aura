using System;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Avalonia.Media.Imaging;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Aura.Desktop.ViewModels;

/// <summary>Avatar images: decoded once per account, shown in place of the initial.</summary>
public partial class MainWindowViewModel
{
    private readonly AvatarCache<Bitmap> _avatars = new(AvatarImage.Decode);

    /// <summary>Largest file we'll read when picking an avatar.</summary>
    public const int MaxAvatarSourceBytes = 25 * 1024 * 1024;

    /// <summary>Our own decoded avatar, or null to show the initial.</summary>
    [ObservableProperty] private Bitmap? _myAvatarImage;

    public bool HasMyAvatar => MyAvatarImage != null;
    public bool ShowMyInitial => MyAvatarImage == null;

    /// <summary>
    /// Asks the user for an image file; null when cancelled. Set by the
    /// window, which owns the storage provider.
    /// </summary>
    public Func<Task<byte[]?>>? AvatarPicker { get; set; }

    partial void OnMyAvatarImageChanged(Bitmap? value)
    {
        OnPropertyChanged(nameof(HasMyAvatar));
        OnPropertyChanged(nameof(ShowMyInitial));
        if (SelfUser != null) SelfUser.Avatar = value;
        if (ProfileCard.IsOpen && ProfileCard.IsSelf) ProfileCard.Avatar = value;
    }

    private async Task ChangeAvatarAsync()
    {
        if (_client == null || AvatarPicker == null) return;
        ProfileCard.Error = "";
        try
        {
            var raw = await AvatarPicker();
            if (raw == null) return;
            var encoded = raw.Length <= MaxAvatarSourceBytes
                ? await Task.Run(() => AvatarImage.Encode(raw))
                : null;
            if (encoded == null)
            {
                ProfileCard.Error = "That file couldn't be read as an image.";
                return;
            }
            await SendAvatarAsync(encoded);
        }
        catch (Exception ex)
        {
            ProfileCard.Status = "";
            ProfileCard.Error = ex.Message;
        }
    }

    private async Task RemoveAvatarAsync()
    {
        if (_client == null) return;
        ProfileCard.Error = "";
        try
        {
            await SendAvatarAsync(Array.Empty<byte>());
        }
        catch (Exception ex)
        {
            ProfileCard.Status = "";
            ProfileCard.Error = ex.Message;
        }
    }

    /// <summary>
    /// Send a profile with this avatar and our stored comment (not the unsaved
    /// draft). The server's reply lands in <see cref="HandleRequestResult"/>
    /// and its broadcast updates <see cref="MyAvatarImage"/>.
    /// </summary>
    private Task SendAvatarAsync(byte[] avatar)
    {
        ProfileCard.Status = "saving…";
        return _client!.UpdateProfileAsync(DisplayName, MyBio, avatar);
    }
}

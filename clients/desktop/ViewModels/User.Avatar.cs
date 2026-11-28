using Avalonia.Media.Imaging;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Aura.Desktop.ViewModels;

/// <summary>A roster row's avatar image; null falls back to the initial.</summary>
public partial class User
{
    [ObservableProperty] private Bitmap? _avatar;

    public bool HasAvatar => Avatar != null;
    public bool ShowInitial => Avatar == null;

    partial void OnAvatarChanged(Bitmap? value)
    {
        OnPropertyChanged(nameof(HasAvatar));
        OnPropertyChanged(nameof(ShowInitial));
    }
}

/// <summary>
/// The sender's avatar beside an incoming message. Our own messages don't use
/// this: their tile binds the view model's avatar directly.
/// </summary>
public partial class ChatMessage
{
    /// <summary>
    /// The sender's account. Session ids change with every connection, so this
    /// is what a later profile update is matched on.
    /// </summary>
    public string? SenderUuid { get; init; }

    [ObservableProperty] private Bitmap? _avatar;

    public bool HasAvatar => Avatar != null;
    public bool ShowInitial => Avatar == null;

    partial void OnAvatarChanged(Bitmap? value)
    {
        OnPropertyChanged(nameof(HasAvatar));
        OnPropertyChanged(nameof(ShowInitial));
    }
}

public partial class ProfileCardViewModel
{
    [ObservableProperty] private Bitmap? _avatar;

    public bool HasAvatar => Avatar != null;
    public bool ShowInitial => Avatar == null;

    /// <summary>Pick a new avatar (own card). Set by the owning view model.</summary>
    public System.Func<System.Threading.Tasks.Task>? ChangeAvatar { get; set; }

    /// <summary>Clear our avatar (own card).</summary>
    public System.Func<System.Threading.Tasks.Task>? RemoveAvatar { get; set; }

    partial void OnAvatarChanged(Bitmap? value)
    {
        OnPropertyChanged(nameof(HasAvatar));
        OnPropertyChanged(nameof(ShowInitial));
        OnPropertyChanged(nameof(CanRemoveAvatar));
    }

    /// <summary>Only our own card, and only when there is one to remove.</summary>
    public bool CanRemoveAvatar => IsSelf && HasAvatar;

    [CommunityToolkit.Mvvm.Input.RelayCommand]
    private async System.Threading.Tasks.Task PickAvatar()
    {
        if (ChangeAvatar != null) await ChangeAvatar();
    }

    [CommunityToolkit.Mvvm.Input.RelayCommand]
    private async System.Threading.Tasks.Task ClearAvatar()
    {
        if (RemoveAvatar != null) await RemoveAvatar();
    }
}

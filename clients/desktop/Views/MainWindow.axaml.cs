using System;
using System.Collections.ObjectModel;
using System.Collections.Specialized;
using System.ComponentModel;
using Aura.Desktop.ViewModels;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Input.Platform;
using Avalonia.Markup.Xaml;
using Avalonia.Media;
using Avalonia.Platform.Storage;
using Avalonia.Threading;
using Aura.Desktop.Services;

namespace Aura.Desktop.Views;

public partial class MainWindow : Window
{
    private MainWindowViewModel? _vm;
    private ObservableCollection<ChatMessage>? _observedMessages;
    private ScrollViewer? _chatScroll;
    private TextBox? _composer;

    /// <summary>
    /// Follow new messages only while the reader is at the bottom; someone
    /// scrolled up to reread must not be yanked back down by every arrival.
    /// </summary>
    private bool _stickToBottom = true;

    public MainWindow()
    {
        InitializeComponent();
        ApplyAcrylicTint();

        _chatScroll = this.FindControl<ScrollViewer>("ChatScroll");
        _composer = this.FindControl<TextBox>("Composer");
        if (_chatScroll != null)
        {
            _chatScroll.ScrollChanged += (_, _) =>
            {
                var sv = _chatScroll;
                _stickToBottom = sv.Offset.Y >= sv.Extent.Height - sv.Viewport.Height - 24;
                if (_stickToBottom) _vm?.ClearMentionsBelow();
            };
        }

        // The voice column's flex width is computed in the view model.
        SizeChanged += (_, e) =>
        {
            if (_vm != null) _vm.WindowWidth = e.NewSize.Width;
        };
        Opened += (_, _) =>
        {
            if (_vm != null) _vm.WindowWidth = Bounds.Width;
        };

        ActualThemeVariantChanged += (_, _) => ApplyAcrylicTint();

        // Key-up never arrives for a key released while another app has focus,
        // so an in-window hold has to end when the window loses focus.
        Deactivated += (_, _) =>
        {
            if (_vm == null) return;
            _vm.IsWindowActive = false;
            if (_vm is { HandlesPushToTalkInWindow: true, IsPushToTalkHeld: true }) _ = _vm.EndPushToTalkAsync();
        };
        Activated += (_, _) =>
        {
            if (_vm != null) _vm.IsWindowActive = true;
        };

        if (this.FindControl<Button>("MentionsBelowPill") is { } pill)
        {
            pill.Click += (_, _) => JumpToFirstMentionBelow();
        }
    }

    private void InitializeComponent()
    {
        AvaloniaXamlLoader.Load(this);
    }

    protected override void OnDataContextChanged(EventArgs e)
    {
        base.OnDataContextChanged(e);

        if (_vm != null)
        {
            _vm.PropertyChanged -= OnViewModelPropertyChanged;
            _vm.AppearanceChanged -= ApplyAcrylicTint;
            _vm.NotificationRequested -= DeliverNotification;
        }

        _vm = DataContext as MainWindowViewModel;
        if (_vm == null) return;

        _vm.PropertyChanged += OnViewModelPropertyChanged;
        _vm.AppearanceChanged += ApplyAcrylicTint;
        _vm.NotificationRequested += DeliverNotification;
        _vm.IsWindowActive = IsActive;
        _vm.ClipboardSetter = async text =>
        {
            if (Clipboard is { } clipboard) await clipboard.SetTextAsync(text);
        };
        _vm.ClipboardGetter = async () =>
            Clipboard is { } clipboard ? await clipboard.TryGetTextAsync() : null;
        _vm.SaveBackupPicker = PickBackupDestinationAsync;
        _vm.OpenBackupPicker = PickBackupSourceAsync;
        _vm.AvatarPicker = PickAvatarSourceAsync;
        _vm.FileAttachmentPicker = PickFileAttachmentAsync;
        _vm.FileSavePicker = PickFileSaveDestinationAsync;
        ObserveMessages(_vm.Messages);
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm == null) return;
        switch (e.PropertyName)
        {
            case nameof(MainWindowViewModel.Messages):
                ObserveMessages(_vm.Messages);
                _stickToBottom = true;
                ScrollChatToEnd();
                break;
            case nameof(MainWindowViewModel.ReplyingTo) when _vm.ReplyingTo != null:
                _composer?.Focus();
                break;
        }
    }

    private void ObserveMessages(ObservableCollection<ChatMessage> messages)
    {
        if (_observedMessages != null) _observedMessages.CollectionChanged -= OnMessagesChanged;
        _observedMessages = messages;
        _observedMessages.CollectionChanged += OnMessagesChanged;
    }

    private void OnMessagesChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        if (e.Action != NotifyCollectionChangedAction.Add) return;
        // Your own message always brings you to the bottom.
        var own = e.NewItems?.Count > 0 && e.NewItems[0] is ChatMessage { IsFromCurrentUser: true };
        if (_stickToBottom || own)
        {
            ScrollChatToEnd();
        }
        else if (e.NewItems?.Count > 0 && e.NewItems[0] is ChatMessage { IsMention: true } mention)
        {
            // Scrolled up reading history: count it in the pill instead.
            _vm?.AddMentionBelow(mention);
        }
    }

    private void JumpToFirstMentionBelow()
    {
        if (_vm?.FirstMentionBelow is not { } target) return;
        if (_chatScroll?.Content is ItemsControl items && items.ContainerFromItem(target) is Control container)
        {
            container.BringIntoView();
        }
        _vm.ConsumeMentionBelow(target);
    }

    /// <summary>
    /// Background delivery, as far as each platform allows. The (N) title
    /// prefix is already bound; this adds the taskbar flash on Windows and a
    /// notify-send banner on Linux. The macOS build of this client is title-only.
    /// </summary>
    private void DeliverNotification(string title, string body)
    {
        if (OperatingSystem.IsWindows())
        {
            DesktopNotifier.FlashTaskbar(TryGetPlatformHandle()?.Handle ?? IntPtr.Zero);
        }
        else if (OperatingSystem.IsLinux())
        {
            DesktopNotifier.ShowLinuxBanner(title, body);
        }
    }

    private static readonly FilePickerFileType BackupFileType = new("Aura identity backup")
    {
        Patterns = new[] { "*.aura-identity" },
    };

    private async System.Threading.Tasks.Task<System.IO.Stream?> PickBackupDestinationAsync()
    {
        var file = await StorageProvider.SaveFilePickerAsync(new FilePickerSaveOptions
        {
            Title = "Save encrypted identity backup",
            SuggestedFileName = $"{(_vm?.DisplayName is { Length: > 0 } n ? n : "aura")}.aura-identity",
            DefaultExtension = "aura-identity",
            FileTypeChoices = new[] { BackupFileType },
        });
        return file == null ? null : await file.OpenWriteAsync();
    }

    private async System.Threading.Tasks.Task<byte[]?> PickBackupSourceAsync()
    {
        var files = await StorageProvider.OpenFilePickerAsync(new FilePickerOpenOptions
        {
            Title = "Restore identity from backup",
            AllowMultiple = false,
            FileTypeFilter = new[] { BackupFileType, FilePickerFileTypes.All },
        });
        if (files.Count == 0) return null;

        await using var stream = await files[0].OpenReadAsync();
        // A backup is a few KB; refuse anything absurd rather than reading it all.
        if (stream.CanSeek && stream.Length > 1024 * 1024) return Array.Empty<byte>();
        using var buffer = new System.IO.MemoryStream();
        await stream.CopyToAsync(buffer);
        return buffer.ToArray();
    }

    private async System.Threading.Tasks.Task<string?> PickFileAttachmentAsync()
    {
        var files = await StorageProvider.OpenFilePickerAsync(new FilePickerOpenOptions
        {
            Title = "Send encrypted file",
            AllowMultiple = false,
            FileTypeFilter = new[] { FilePickerFileTypes.All },
        });
        return files.Count == 0 ? null : files[0].TryGetLocalPath();
    }

    private async System.Threading.Tasks.Task<System.IO.Stream?> PickFileSaveDestinationAsync(string suggestedName)
    {
        var file = await StorageProvider.SaveFilePickerAsync(new FilePickerSaveOptions
        {
            Title = "Save file",
            SuggestedFileName = suggestedName,
        });
        return file == null ? null : await file.OpenWriteAsync();
    }

    private async System.Threading.Tasks.Task<byte[]?> PickAvatarSourceAsync()
    {
        var files = await StorageProvider.OpenFilePickerAsync(new FilePickerOpenOptions
        {
            Title = "Choose an avatar",
            AllowMultiple = false,
            FileTypeFilter = new[] { FilePickerFileTypes.ImageAll },
        });
        if (files.Count == 0) return null;

        await using var stream = await files[0].OpenReadAsync();
        // Photos can be large, but not unbounded: past the cap, hand back an
        // empty array so the caller reports "couldn't read" instead of loading it.
        if (stream.CanSeek && stream.Length > MainWindowViewModel.MaxAvatarSourceBytes) return Array.Empty<byte>();
        using var buffer = new System.IO.MemoryStream();
        await stream.CopyToAsync(buffer);
        return buffer.ToArray();
    }

    private void ScrollChatToEnd() =>
        // After layout, so the new item has a height to scroll to.
        Dispatcher.UIThread.Post(() => _chatScroll?.ScrollToEnd(), DispatcherPriority.Background);

    /// <summary>
    /// Tint the window material from the palette. Set in code because the acrylic
    /// material is not part of the visual tree and can't resolve DynamicResource.
    /// </summary>
    private void ApplyAcrylicTint()
    {
        var border = this.FindControl<ExperimentalAcrylicBorder>("AcrylicRoot");
        if (border == null) return;

        var app = Application.Current;
        var tint = Color.Parse("#0A0A0D");
        if (app != null &&
            app.Resources.TryGetResource("AuraBackgroundColor", ActualThemeVariant, out var value) &&
            value is Color themed)
        {
            tint = themed;
        }

        border.Material = new ExperimentalAcrylicMaterial
        {
            BackgroundSource = AcrylicBackgroundSource.Digger,
            TintColor = tint,
            TintOpacity = 1,
            MaterialOpacity = 0.72,
            FallbackColor = tint,
        };
    }

    protected override void OnKeyDown(KeyEventArgs e)
    {
        if (_vm == null)
        {
            base.OnKeyDown(e);
            return;
        }

        // Recording a new push-to-talk key in Settings takes every key.
        if (_vm.TryRecordPttKey(e.Key, e.KeyModifiers))
        {
            e.Handled = true;
            return;
        }

        if (e.Key == Key.Escape && CloseTopOverlay())
        {
            e.Handled = true;
            return;
        }

        // In-window push to talk where there is no global hook. A binding with
        // no modifiers is a key people also type, so it doesn't fire in a text box.
        if (_vm.HandlesPushToTalkInWindow && _vm.PttBinding.Matches(e.Key, e.KeyModifiers) &&
            !(FocusManager?.GetFocusedElement() is TextBox && _vm.PttBinding.Modifiers == KeyModifiers.None))
        {
            e.Handled = true;
            _ = _vm.BeginPushToTalkAsync();
            return;
        }

        base.OnKeyDown(e);
    }

    protected override void OnKeyUp(KeyEventArgs e)
    {
        if (_vm is { HandlesPushToTalkInWindow: true, IsPushToTalkHeld: true } && e.Key == _vm.PttBinding.Key)
        {
            e.Handled = true;
            _ = _vm.EndPushToTalkAsync();
            return;
        }

        base.OnKeyUp(e);
    }

    /// <summary>Esc closes the topmost overlay, in stacking order.</summary>
    private bool CloseTopOverlay()
    {
        if (_vm == null) return false;
        if (_vm.ChannelEditor.IsOpen) { _vm.ChannelEditor.Close(); return true; }
        if (_vm.ProfileCard.IsOpen) { _vm.ProfileCard.Close(); return true; }
        if (_vm.ShowSettings) { _vm.ToggleSettingsCommand.Execute(null); return true; }
        if (_vm.Moderation.IsOpen) { _vm.Moderation.CloseCommand.Execute(null); return true; }
        if (_vm.TrustSheet.IsOpen) { _vm.TrustSheet.CloseCommand.Execute(null); return true; }
        return false;
    }
}

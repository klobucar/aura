using System;
using System.Collections.ObjectModel;
using System.Collections.Specialized;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using uniffi.aura_core;

namespace Aura.Desktop.ViewModels;

/// <summary>Chat: per-channel history, sending, replies, copy, retry.</summary>
public partial class MainWindowViewModel
{
    private readonly ChatHistory _history = new();

    /// <summary>The selected channel's messages (swapped on channel switch).</summary>
    [ObservableProperty] private ObservableCollection<ChatMessage> _messages = new();

    [ObservableProperty] private string _messageInput = "";

    /// <summary>The message the composer is replying to, if any.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsReplying))]
    [NotifyPropertyChangedFor(nameof(ReplyBarTitle))]
    [NotifyPropertyChangedFor(nameof(ReplyBarPreview))]
    private ChatMessage? _replyingTo;

    public bool IsReplying => ReplyingTo != null;
    public string ReplyBarTitle => ReplyingTo == null ? "" : $"Replying to {ReplyingTo.UserName}";
    public string ReplyBarPreview => ReplyingTo == null ? "" : ChatMessage.PreviewOf(ReplyingTo.Content);

    /// <summary>`12 messages today`</summary>
    [ObservableProperty] private string _messagesTodayLabel = "0 messages today";

    /// <summary>
    /// The chat column's empty state. Join notices alone still leave the
    /// column looking unused, so they do not count as content.
    /// </summary>
    [ObservableProperty] private bool _showChatEmptyState = true;

    /// <summary>`Message #Lounge…`</summary>
    public string ComposerPlaceholder =>
        SelectedChannel == null ? "Message…" : $"Message #{SelectedChannel.Name}…";

    /// <summary>Channel comment / MOTD, pinned above chat when set (§6).</summary>
    public string ChannelMotd => SelectedChannel?.Comment ?? "";
    public bool HasChannelMotd => !string.IsNullOrWhiteSpace(ChannelMotd);
    public string ChannelMotdTitle => SelectedChannel == null ? "" : $"{SelectedChannel.Icon} {SelectedChannel.Name}";

    /// <summary>Initial on our own messages' avatar.</summary>
    public string OwnInitial => string.IsNullOrWhiteSpace(DisplayName) ? "?" : DisplayName.Trim()[..1].ToUpperInvariant();

    partial void OnDisplayNameChanged(string value)
    {
        OnPropertyChanged(nameof(OwnInitial));
        UpdateSelfRow();
    }

    /// <summary>Set by the view; the VM has no clipboard of its own.</summary>
    public Func<string, Task>? ClipboardSetter { get; set; }

    /// <summary>Set by the view to pick a local file to send.</summary>
    public Func<Task<string?>>? FileAttachmentPicker { get; set; }

    /// <summary>Set by the view to pick a destination stream when saving a received file.</summary>
    public Func<string, Task<Stream?>>? FileSavePicker { get; set; }

    private ObservableCollection<ChatMessage>? _observedMessages;

    private void ShowMessagesFor(string? channelId)
    {
        if (_observedMessages != null) _observedMessages.CollectionChanged -= OnMessagesChanged;
        _observedMessages = _history.For(channelId);
        _observedMessages.CollectionChanged += OnMessagesChanged;
        Messages = _observedMessages;
        OnPropertyChanged(nameof(ChannelMotdTitle));
        RefreshMessageCount();
    }

    private void OnMessagesChanged(object? sender, NotifyCollectionChangedEventArgs e) => RefreshMessageCount();

    private void RefreshMessageCount()
    {
        var today = DateTime.Now.Date;
        var count = Messages.Count(m => !m.System && m.Timestamp.Date == today);
        MessagesTodayLabel = count == 1 ? "1 message today" : $"{count} messages today";
        ShowChatEmptyState = !Messages.Any(m => !m.System);
    }

    /// <summary>A centred system line in one channel's history.</summary>
    private void AddNotice(string channelId, string text) =>
        _history.Add(new ChatMessage { ChannelId = channelId, Content = text, System = true });

    /// <summary>Give a message its per-item commands (context menus bind to the item).</summary>
    private ChatMessage Wire(ChatMessage message)
    {
        message.ReplyCommand = new RelayCommand(() => ReplyingTo = message);
        message.CopyCommand = new AsyncRelayCommand(() => CopyAsync(message));
        message.RetryCommand = new AsyncRelayCommand(() => SendAsync(message));
        message.SaveAttachmentCommand = new AsyncRelayCommand(() => SaveAttachmentAsync(message));
        return message;
    }

    private async Task SaveAttachmentAsync(ChatMessage message)
    {
        if (FileSavePicker == null || string.IsNullOrEmpty(message.AttachmentLocalPath) || !File.Exists(message.AttachmentLocalPath))
            return;

        var suggestedName = string.IsNullOrWhiteSpace(message.AttachmentFileName) ? "attachment" : message.AttachmentFileName;
        await using var dest = await FileSavePicker(suggestedName);
        if (dest == null) return;

        await using var src = File.OpenRead(message.AttachmentLocalPath);
        await src.CopyToAsync(dest);
    }

    [RelayCommand]
    private async Task AttachFileAsync()
    {
        var targetChannel = SelectedChannel;
        if (_client == null || targetChannel == null)
        {
            ShowBanner("Join a channel before sending files.", isError: true);
            return;
        }
        if (FileAttachmentPicker == null) return;

        var path = await FileAttachmentPicker();
        if (string.IsNullOrEmpty(path) || !File.Exists(path)) return;

        var info = new FileInfo(path);
        var maxBytes = AuraCoreMethods.MaxFilePlaintextSize();
        if ((ulong)info.Length > maxBytes)
        {
            ShowBanner($"File is too large (max {maxBytes / (1024 * 1024)} MiB).", isError: true);
            return;
        }

        var caption = MessageInput.TrimEnd();
        var replying = ReplyingTo;
        MessageInput = "";
        ReplyingTo = null;

        var fileIdHex = Guid.NewGuid().ToString("N");
        var message = Wire(new ChatMessage
        {
            ChannelId = targetChannel.Id,
            UserId = _client.UserId,
            UserName = "you",
            Content = caption,
            IsFromCurrentUser = true,
            ReplyToId = replying?.Id,
            ReplySender = replying == null ? "" : replying.IsFromCurrentUser ? "you" : replying.UserName,
            ReplyPreview = replying == null ? "" : ChatMessage.PreviewOf(replying.Content),
            AttachmentFileIdHex = fileIdHex,
            AttachmentFileName = info.Name,
            AttachmentSize = (ulong)info.Length,
            AttachmentStatus = "Encrypting…",
            AttachmentLocalPath = path,
        });
        TryLoadImagePreview(message, path, info.Name, null);
        _history.Add(message);

        try
        {
            await _client.SendFileAsync(
                targetChannel.Id,
                path,
                caption,
                message.Id,
                message.ReplyToId,
                status => Avalonia.Threading.Dispatcher.UIThread.Post(() => message.AttachmentStatus = status));
        }
        catch (Exception ex)
        {
            message.SendError = ex.Message;
            message.AttachmentStatus = "Failed";
        }
    }

    private async Task CopyAsync(ChatMessage message)
    {
        if (ClipboardSetter != null) await ClipboardSetter(message.Content);
    }

    [RelayCommand]
    private void CancelReply() => ReplyingTo = null;

    [RelayCommand]
    private async Task SendMessage()
    {
        if (string.IsNullOrWhiteSpace(MessageInput)) return;

        if (_client == null || SelectedChannel == null)
        {
            // Without a channel there is nowhere to put the message; say so
            // instead of silently dropping what was typed.
            ShowBanner("Join a channel before sending messages.", isError: true);
            return;
        }

        var content = MessageInput.TrimEnd();
        var replying = ReplyingTo;
        MessageInput = "";
        ReplyingTo = null;

        var message = Wire(new ChatMessage
        {
            ChannelId = SelectedChannel.Id,
            UserId = _client.UserId,
            UserName = "you",
            Content = content,
            IsFromCurrentUser = true,
            ReplyToId = replying?.Id,
            ReplySender = replying == null ? "" : replying.IsFromCurrentUser ? "you" : replying.UserName,
            ReplyPreview = replying == null ? "" : ChatMessage.PreviewOf(replying.Content),
        });

        // Optimistic: it shows immediately, and a failure is marked on the
        // bubble with a retry rather than vanishing.
        _history.Add(message);
        await SendAsync(message);
    }

    private async Task SendAsync(ChatMessage message)
    {
        if (_client == null)
        {
            message.SendError = "not connected";
            return;
        }

        message.SendError = "";
        try
        {
            await _client.SendTextMessageAsync(message.ChannelId, message.Content, message.Id, message.ReplyToId);
        }
        catch (Exception ex)
        {
            message.SendError = ex.Message;
        }
    }

    private void HandleTextMessage(string msgId, uint senderId, string channelId, string content, string? replyToId)
    {
        if (_client != null && senderId == _client.UserId) return;

        var sender = Channels.SelectMany(c => c.Users).FirstOrDefault(u => !u.IsSelf && u.Id == senderId);
        var quoted = _history.Find(channelId, replyToId);

        var message = Wire(new ChatMessage
        {
            Id = string.IsNullOrEmpty(msgId) ? Guid.NewGuid().ToString() : msgId,
            ChannelId = channelId,
            UserId = senderId,
            UserName = sender?.Name ?? $"User {senderId}",
            Content = content,
            ReplyToId = replyToId,
            ReplySender = quoted == null ? "" : quoted.IsFromCurrentUser ? "you" : quoted.UserName,
            ReplyPreview = quoted == null ? "" : ChatMessage.PreviewOf(quoted.Content),
            IsMention = IsMentionForMe(content),
        });

        if (_history.Add(message)) NotifyForMessage(message);
    }

    private void HandleFileAnnouncement(
        string msgId,
        uint senderId,
        string channelId,
        string caption,
        string? replyToId,
        Services.AuraNetworkClient.FileAttachmentInfo attachment)
    {
        if (_client != null && senderId == _client.UserId) return;

        var sender = Channels.SelectMany(c => c.Users).FirstOrDefault(u => !u.IsSelf && u.Id == senderId);
        var quoted = _history.Find(channelId, replyToId);

        var message = Wire(new ChatMessage
        {
            Id = string.IsNullOrEmpty(msgId) ? Guid.NewGuid().ToString() : msgId,
            ChannelId = channelId,
            UserId = senderId,
            UserName = sender?.Name ?? $"User {senderId}",
            Content = caption,
            ReplyToId = replyToId,
            ReplySender = quoted == null ? "" : quoted.IsFromCurrentUser ? "you" : quoted.UserName,
            ReplyPreview = quoted == null ? "" : ChatMessage.PreviewOf(quoted.Content),
            IsMention = IsMentionForMe(caption),
            AttachmentFileIdHex = attachment.FileIdHex,
            AttachmentFileName = attachment.FileName,
            AttachmentMimeType = attachment.MimeType,
            AttachmentSize = attachment.Size,
            AttachmentStatus = "Receiving…",
        });

        if (_history.Add(message)) NotifyForMessage(message);
    }

    private void HandleFileTransferUpdated(string channelId, string fileIdHex, string status, string? localPath)
    {
        var list = _history.For(channelId);
        var msg = list.FirstOrDefault(m => m.AttachmentFileIdHex == fileIdHex);
        if (msg == null) return;

        msg.AttachmentStatus = status;
        if (!string.IsNullOrEmpty(localPath))
        {
            msg.AttachmentLocalPath = localPath;
            TryLoadImagePreview(msg, localPath, msg.AttachmentFileName, msg.AttachmentMimeType);
        }
    }

    private static void TryLoadImagePreview(ChatMessage msg, string localPath, string? fileName, string? mimeType)
    {
        const ulong maxPreviewBytes = 10 * 1024 * 1024;
        if (msg.AttachmentSize > maxPreviewBytes) return;

        var ext = Path.GetExtension(fileName ?? localPath).ToLowerInvariant();
        var isImage = (mimeType?.StartsWith("image/", StringComparison.OrdinalIgnoreCase) ?? false)
            || ext is ".png" or ".jpg" or ".jpeg" or ".gif" or ".bmp" or ".webp";
        if (!isImage) return;

        _ = Task.Run(() =>
        {
            try
            {
                using var fs = File.OpenRead(localPath);
                var bitmap = Avalonia.Media.Imaging.Bitmap.DecodeToWidth(fs, 512);
                Avalonia.Threading.Dispatcher.UIThread.Post(() => msg.AttachmentPreview = bitmap);
            }
            catch
            {
                // Non-decodable image; leave as a standard file card.
            }
        });
    }
}

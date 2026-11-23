using System;
using System.Collections.ObjectModel;
using System.Collections.Specialized;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

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
        return message;
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
}

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using Aura.Desktop.Services;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Aura.Desktop.ViewModels;

/// <summary>One chat row: a message, or a centred system notice.</summary>
public partial class ChatMessage : ObservableObject
{
    /// <summary>Wire message id (for replies and de-duplication).</summary>
    public string Id { get; init; } = Guid.NewGuid().ToString();

    /// <summary>The channel this row belongs to; "" for notices outside any channel.</summary>
    public string ChannelId { get; init; } = "";

    public uint UserId { get; init; }
    public string UserName { get; init; } = "";
    public string Content { get; init; } = "";
    public DateTime Timestamp { get; init; } = DateTime.Now;
    public bool IsFromCurrentUser { get; init; }
    public bool System { get; init; }

    /// <summary>Mentions us (by @name or a notify keyword): caution tint + `mentions you`.</summary>
    public bool IsMention { get; init; }

    /// <summary>
    /// The "Joined #channel" notice for our own arrival. A channel keeps only
    /// the latest one (see <see cref="ChatHistory.AddJoinMarker"/>), so it
    /// marks where this visit's history starts.
    /// </summary>
    public bool IsJoinMarker { get; init; }

    /// <summary>The message this one replies to, if any.</summary>
    public string? ReplyToId { get; init; }

    /// <summary>Who wrote the quoted message (`you` / a name), mono in the bubble.</summary>
    public string ReplySender { get; init; } = "";

    /// <summary>First line of the quoted message, plain text.</summary>
    public string ReplyPreview { get; init; } = "";

    /// <summary>
    /// The reply context is drawn only when the quoted message is known; a bare
    /// "reply to something" with nothing to show would just be noise.
    /// </summary>
    public bool HasReply => !string.IsNullOrEmpty(ReplyToId) && !string.IsNullOrEmpty(ReplyPreview);

    /// <summary>Why an outgoing message did not go out; empty when it did.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SendFailed))]
    private string _sendError = "";

    public bool SendFailed => !string.IsNullOrEmpty(SendError);

    /// <summary>`not sent · reason`, shown under a failed bubble beside Retry.</summary>
    public string SendErrorLabel => SendFailed ? $"not sent · {SendError}" : "";

    partial void OnSendErrorChanged(string value)
    {
        OnPropertyChanged(nameof(SendErrorLabel));
        OnPropertyChanged(nameof(BubbleOpacity));
    }

    // Set by the owning view model. Context menus live in a popup outside the
    // window's visual tree, so they bind to the item instead of an ancestor.
    public CommunityToolkit.Mvvm.Input.IRelayCommand? ReplyCommand { get; set; }
    public CommunityToolkit.Mvvm.Input.IRelayCommand? CopyCommand { get; set; }
    public CommunityToolkit.Mvvm.Input.IRelayCommand? RetryCommand { get; set; }
    public CommunityToolkit.Mvvm.Input.IRelayCommand? SaveAttachmentCommand { get; set; }

    // ── E2EE File Attachment ─────────────────────────────────────────────────
    public string? AttachmentFileIdHex { get; init; }
    public string? AttachmentFileName { get; init; }
    public string? AttachmentMimeType { get; init; }
    public ulong AttachmentSize { get; init; }

    public bool HasAttachment => !string.IsNullOrEmpty(AttachmentFileName);

    public string AttachmentSizeLabel => FormatBytes(AttachmentSize);

    [ObservableProperty]
    private string _attachmentStatus = "";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CanSaveAttachment))]
    private string? _attachmentLocalPath;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasAttachmentPreview))]
    private Avalonia.Media.Imaging.Bitmap? _attachmentPreview;

    public bool HasAttachmentPreview => AttachmentPreview != null;
    public bool CanSaveAttachment => !string.IsNullOrEmpty(AttachmentLocalPath);

    private static string FormatBytes(ulong bytes) => bytes switch
    {
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024.0:0.#} KB",
        _ => $"{bytes / (1024.0 * 1024.0):0.##} MB",
    };

    /// <summary>A message from someone else (avatar on the left).</summary>
    public bool IsIncoming => !System && !IsFromCurrentUser;

    /// <summary>Incoming left, outgoing right (row-reverse), notices centred.</summary>
    public Avalonia.Layout.HorizontalAlignment Alignment =>
        System ? Avalonia.Layout.HorizontalAlignment.Center
        : IsFromCurrentUser ? Avalonia.Layout.HorizontalAlignment.Right
        : Avalonia.Layout.HorizontalAlignment.Left;

    /// <summary>A message that didn't go out is drawn dimmed above its error.</summary>
    public double BubbleOpacity => SendFailed ? 0.55 : 1.0;

    /// <summary>Initial for the 34px avatar beside the message.</summary>
    public string Initial => string.IsNullOrWhiteSpace(UserName) ? "?" : UserName.Trim()[..1].ToUpperInvariant();

    /// <summary>`20:14`, mono, on the name row.</summary>
    public string TimeLabel => Timestamp.ToString("HH:mm");

    /// <summary>The quoted line, as it appears in reply context and the reply bar.</summary>
    public static string PreviewOf(string content)
    {
        var plain = InlineMarkdown.PlainText(content).Replace('\n', ' ').Trim();
        return plain.Length <= 80 ? plain : plain[..80] + "…";
    }
}

/// <summary>
/// Chat history kept per channel, so switching channels shows that channel's
/// conversation instead of one stream with every room mixed together. Bounded
/// per channel: history is in-memory only (the server stores no messages), and
/// a busy room left open all day must not grow without limit.
/// </summary>
public sealed class ChatHistory
{
    public const int MaxPerChannel = 500;

    private readonly Dictionary<string, ObservableCollection<ChatMessage>> _byChannel = new();

    /// <summary>The live collection for a channel; created on first use.</summary>
    public ObservableCollection<ChatMessage> For(string? channelId)
    {
        var key = channelId ?? "";
        if (!_byChannel.TryGetValue(key, out var list))
        {
            list = new ObservableCollection<ChatMessage>();
            _byChannel[key] = list;
        }
        return list;
    }

    /// <summary>
    /// Append a message to its channel. Returns false for a duplicate id, which
    /// is how a reconnect replay or an echo of our own message is dropped.
    /// </summary>
    public bool Add(ChatMessage message)
    {
        var list = For(message.ChannelId);
        if (!message.System && list.Any(m => m.Id == message.Id)) return false;

        list.Add(message);
        while (list.Count > MaxPerChannel) list.RemoveAt(0);
        return true;
    }

    /// <summary>
    /// Record that we joined a channel, dropping the channel's earlier join
    /// marker. Hopping between channels, or a reconnect rejoining, would
    /// otherwise stack a "Joined #channel" for every visit.
    /// </summary>
    public void AddJoinMarker(ChatMessage marker)
    {
        var list = For(marker.ChannelId);
        for (var i = list.Count - 1; i >= 0; i--)
        {
            if (list[i].IsJoinMarker) list.RemoveAt(i);
        }
        Add(marker);
    }

    public ChatMessage? Find(string? channelId, string? messageId)
    {
        if (string.IsNullOrEmpty(messageId)) return null;
        return _byChannel.TryGetValue(channelId ?? "", out var list)
            ? list.FirstOrDefault(m => m.Id == messageId)
            : null;
    }

    /// <summary>Every message held, across all channels.</summary>
    public IEnumerable<ChatMessage> All => _byChannel.Values.SelectMany(list => list);

    /// <summary>Forget a deleted channel's history.</summary>
    public void Remove(string channelId) => _byChannel.Remove(channelId);

    public void Clear()
    {
        foreach (var list in _byChannel.Values) list.Clear();
    }
}

using System;
using System.Collections.Generic;
using System.Linq;
using Aura.Desktop.Services;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Aura.Desktop.ViewModels;

/// <summary>Mentions and background notifications.</summary>
public partial class MainWindowViewModel
{
    public IReadOnlyList<string> NotifyLevelOptions { get; } = new[] { "Everything", "Mentions", "Nothing" };

    [ObservableProperty] private int _notifyLevelIndex = (int)NotifyLevel.Mentions;
    [ObservableProperty] private string _notifyKeywords = "";
    [ObservableProperty] private bool _notifyShowText = true;
    [ObservableProperty] private bool _notifyBadge = true;
    [ObservableProperty] private bool _notifyJoinLeave;

    private NotifyLevel NotifyLevel => (NotifyLevel)Math.Clamp(NotifyLevelIndex, 0, 2);
    private IReadOnlyList<string> _keywords = Array.Empty<string>();

    partial void OnNotifyLevelIndexChanged(int value) => SaveSettings();
    partial void OnNotifyKeywordsChanged(string value)
    {
        _keywords = Mentions.ParseKeywords(value);
        SaveSettings();
    }
    partial void OnNotifyShowTextChanged(bool value) => SaveSettings();
    partial void OnNotifyBadgeChanged(bool value)
    {
        OnPropertyChanged(nameof(WindowTitle));
        SaveSettings();
    }
    partial void OnNotifyJoinLeaveChanged(bool value) => SaveSettings();

    /// <summary>Set by the window on activate/deactivate. Notifications only go out while inactive.</summary>
    [ObservableProperty] private bool _isWindowActive = true;

    /// <summary>Notifications delivered since the window was last active.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(WindowTitle))]
    private int _unseenCount;

    /// <summary>`(N) Aura` while there's something unseen and the count is enabled.</summary>
    public string WindowTitle => NotifyBadge && UnseenCount > 0 ? $"({UnseenCount}) Aura" : "Aura";

    partial void OnIsWindowActiveChanged(bool value)
    {
        if (value) UnseenCount = 0;
    }

    /// <summary>
    /// A banner to show (title, body). The window delivers it in whatever way
    /// the platform allows (flash on Windows, notify-send on Linux).
    /// </summary>
    public event Action<string, string>? NotificationRequested;

    /// <summary>Whether this incoming message mentions us (by @name or keyword).</summary>
    private bool IsMentionForMe(string content) =>
        Mentions.IsMention(content, DisplayName, _keywords, isOwnMessage: false);

    /// <summary>An incoming chat message arrived; notify if the settings say so.</summary>
    private void NotifyForMessage(ChatMessage message)
    {
        if (IsWindowActive || message.System || message.IsFromCurrentUser) return;
        if (!NotifyLevels.ShouldNotify(NotifyLevel, message.IsMention)) return;

        var channel = Channels.FirstOrDefault(c => c.Id == message.ChannelId)?.Name ?? "a channel";
        UnseenCount++;

        string body;
        if (NotifyShowText)
        {
            body = $"{message.UserName}: {ChatMessage.PreviewOf(message.Content)}";
        }
        else
        {
            body = message.IsMention ? $"New mention in {channel}" : $"New message in {channel}";
        }
        NotificationRequested?.Invoke("Aura", body);
    }

    /// <summary>Join/leave banners, off by default; they never count as unseen.</summary>
    private void NotifyForPresence(string text)
    {
        if (IsWindowActive || !NotifyJoinLeave || NotifyLevel == NotifyLevel.Nothing) return;
        NotificationRequested?.Invoke("Aura", text);
    }

    // ── "N mentions below" pill ──

    private readonly List<ChatMessage> _mentionsBelow = new();

    [ObservableProperty] private string _mentionsBelowLabel = "";
    public bool HasMentionsBelow => _mentionsBelow.Count > 0;

    /// <summary>The first mention the reader hasn't scrolled to yet.</summary>
    public ChatMessage? FirstMentionBelow => _mentionsBelow.FirstOrDefault();

    /// <summary>Called by the view when a mention lands below the viewport.</summary>
    public void AddMentionBelow(ChatMessage message)
    {
        if (!message.IsMention || _mentionsBelow.Contains(message)) return;
        _mentionsBelow.Add(message);
        RefreshMentionsBelow();
    }

    /// <summary>The pill took the reader to this mention.</summary>
    public void ConsumeMentionBelow(ChatMessage message)
    {
        if (_mentionsBelow.Remove(message)) RefreshMentionsBelow();
    }

    /// <summary>Called when the reader reaches the bottom or switches channel.</summary>
    public void ClearMentionsBelow()
    {
        if (_mentionsBelow.Count == 0) return;
        _mentionsBelow.Clear();
        RefreshMentionsBelow();
    }

    private void RefreshMentionsBelow()
    {
        var n = _mentionsBelow.Count;
        MentionsBelowLabel = n == 1 ? "1 mention below" : $"{n} mentions below";
        OnPropertyChanged(nameof(HasMentionsBelow));
        OnPropertyChanged(nameof(FirstMentionBelow));
    }
}

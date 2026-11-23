using System;
using System.Collections.Generic;
using System.Linq;

namespace Aura.Desktop.Services;

/// <summary>
/// Mention detection, shared rules with the macOS client:
/// a message mentions you if it contains <c>@&lt;your display name&gt;</c>
/// (case-insensitive) followed by the end, whitespace or punctuation, or any
/// configured keyword as a whole word (case-insensitive). Your own messages
/// never mention you.
/// </summary>
public static class Mentions
{
    public static bool IsMention(string? content, string? displayName, IEnumerable<string>? keywords, bool isOwnMessage)
    {
        if (isOwnMessage || string.IsNullOrEmpty(content)) return false;
        if (FindNameMentions(content, displayName).Count > 0) return true;

        foreach (var keyword in keywords ?? Array.Empty<string>())
        {
            if (ContainsWholeWord(content, keyword)) return true;
        }
        return false;
    }

    /// <summary>
    /// Every <c>@name</c> token (start, length including the @) — the ranges the
    /// renderer draws as caution pills.
    /// </summary>
    public static IReadOnlyList<(int Start, int Length)> FindNameMentions(string? content, string? displayName)
    {
        var found = new List<(int, int)>();
        var name = displayName?.Trim();
        if (string.IsNullOrEmpty(content) || string.IsNullOrEmpty(name)) return found;

        var token = "@" + name;
        int from = 0;
        while (from <= content.Length - token.Length)
        {
            var at = content.IndexOf(token, from, StringComparison.OrdinalIgnoreCase);
            if (at < 0) break;
            var after = at + token.Length;
            if (after == content.Length || IsBoundary(content[after]))
            {
                found.Add((at, token.Length));
                from = after;
            }
            else
            {
                from = at + 1;
            }
        }
        return found;
    }

    /// <summary>Whether <paramref name="word"/> appears with a non-word character (or nothing) on each side.</summary>
    public static bool ContainsWholeWord(string content, string? word)
    {
        var w = word?.Trim();
        if (string.IsNullOrEmpty(w)) return false;

        int from = 0;
        while (from <= content.Length - w.Length)
        {
            var at = content.IndexOf(w, from, StringComparison.OrdinalIgnoreCase);
            if (at < 0) return false;
            var before = at == 0 || !IsWordChar(content[at - 1]);
            var afterIdx = at + w.Length;
            var after = afterIdx == content.Length || !IsWordChar(content[afterIdx]);
            if (before && after) return true;
            from = at + 1;
        }
        return false;
    }

    /// <summary>Split the settings text "a, b ,c" into keywords.</summary>
    public static IReadOnlyList<string> ParseKeywords(string? text) =>
        (text ?? "").Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Where(k => k.Length > 0)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .ToList();

    private static bool IsBoundary(char c) => char.IsWhiteSpace(c) || char.IsPunctuation(c);

    private static bool IsWordChar(char c) => char.IsLetterOrDigit(c) || c == '_';
}

/// <summary>Who gets notified about what. Persisted as <c>AuraNotifyLevel</c>.</summary>
public enum NotifyLevel
{
    Everything,
    Mentions,
    Nothing,
}

public static class NotifyLevels
{
    public static string ToSetting(NotifyLevel level) => level switch
    {
        NotifyLevel.Everything => "all",
        NotifyLevel.Nothing => "none",
        _ => "mentions",
    };

    public static NotifyLevel Parse(string? value) => value switch
    {
        "all" => NotifyLevel.Everything,
        "none" => NotifyLevel.Nothing,
        _ => NotifyLevel.Mentions,
    };

    /// <summary>Whether a message at this level should notify.</summary>
    public static bool ShouldNotify(NotifyLevel level, bool isMention) => level switch
    {
        NotifyLevel.Everything => true,
        NotifyLevel.Mentions => isMention,
        _ => false,
    };
}

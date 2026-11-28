using System;
using System.Collections.Generic;
using System.Text;

namespace Aura.Desktop.Services;

/// <summary>Inline styles a span can carry. Flags, because bold and italic nest.</summary>
[Flags]
public enum MdStyle
{
    None = 0,
    Bold = 1,
    Italic = 2,
    Code = 4,
}

/// <summary>One run of text with its style and, for links, its target.</summary>
public sealed record MdSpan(string Text, MdStyle Style, string? Link = null)
{
    /// <summary>
    /// Only web links are followed. Anything else (aura://, javascript:, file:)
    /// still renders as a link-coloured label but opens nothing.
    /// </summary>
    public bool IsOpenableLink =>
        Link != null &&
        Uri.TryCreate(Link, UriKind.Absolute, out var uri) &&
        (uri.Scheme == Uri.UriSchemeHttp || uri.Scheme == Uri.UriSchemeHttps);
}

public enum MdBlockKind
{
    Paragraph,
    Bullet,
    Quote,
    CodeBlock,
}

/// <summary>A line-level block for comment cards: paragraph, bullet, or quote.</summary>
public sealed record MdBlock(MdBlockKind Kind, IReadOnlyList<MdSpan> Spans);

/// <summary>
/// The markdown subset the handoff allows (§6 "Comment content rules"): bold,
/// italic, inline code, inline links, and — for comment cards — unordered lists
/// and blockquotes. No headings, tables, raw HTML or images: anything else is
/// shown as the literal text it is. Chat messages use the inline subset only,
/// matching the macOS client's <c>inlineOnlyPreservingWhitespace</c>.
///
/// Rendering is the view's job (<c>Controls/MarkdownText</c>); this is pure
/// parsing so it can be tested without a UI.
/// </summary>
public static class InlineMarkdown
{
    /// <summary>
    /// Longest text that gets markdown at all; anything longer renders as
    /// plain text. Matches the composer's cap. A peer controls message content
    /// and the server relays up to 256 KB, so parsing must stay cheap on the
    /// UI thread whatever arrives.
    /// </summary>
    public const int MaxMarkdownLength = 4000;

    /// <summary>Parse inline markdown into styled spans, merging neighbours with the same style.</summary>
    public static IReadOnlyList<MdSpan> ParseInline(string? text)
    {
        var spans = new List<MdSpan>();
        if (string.IsNullOrEmpty(text)) return spans;
        if (text.Length > MaxMarkdownLength)
        {
            spans.Add(new MdSpan(text, MdStyle.None));
            return spans;
        }
        Parse(text, 0, text.Length, MdStyle.None, null, spans);
        return Merge(spans);
    }

    /// <summary>
    /// Split comment text into blocks: <c>- </c>/<c>* </c> lines are bullets,
    /// <c>&gt; </c> lines are quotes, everything else is a paragraph line.
    /// </summary>
    public static IReadOnlyList<MdBlock> ParseBlocks(string? text)
    {
        var blocks = new List<MdBlock>();
        if (string.IsNullOrEmpty(text)) return blocks;

        var lines = text.Replace("\r\n", "\n").Split('\n');
        for (int i = 0; i < lines.Length; i++)
        {
            var line = lines[i].TrimEnd();
            var trimmed = line.TrimStart();
            if (trimmed.Length == 0) continue;

            if (trimmed.StartsWith("```", StringComparison.Ordinal))
            {
                var tag = trimmed[3..].Trim();
                var codeLines = new List<string>();
                if (tag.Length > 0 && !IsLanguageTag(tag))
                {
                    codeLines.Add(trimmed[3..]);
                }
                int j = i + 1;
                for (; j < lines.Length; j++)
                {
                    if (lines[j].TrimStart().StartsWith("```", StringComparison.Ordinal))
                    {
                        break;
                    }
                    codeLines.Add(lines[j]);
                }
                blocks.Add(new MdBlock(MdBlockKind.CodeBlock, new[]
                {
                    new MdSpan(string.Join("\n", codeLines), MdStyle.Code),
                }));
                i = j;
                continue;
            }

            if (trimmed.StartsWith("- ") || trimmed.StartsWith("* "))
            {
                blocks.Add(new MdBlock(MdBlockKind.Bullet, ParseInline(trimmed[2..])));
            }
            else if (trimmed.StartsWith(">"))
            {
                blocks.Add(new MdBlock(MdBlockKind.Quote, ParseInline(trimmed[1..].TrimStart())));
            }
            else
            {
                blocks.Add(new MdBlock(MdBlockKind.Paragraph, ParseInline(line)));
            }
        }
        return blocks;
    }

    /// <summary>Concatenated plain text, for previews and copy.</summary>
    public static string PlainText(string? text)
    {
        if (text is { Length: > MaxMarkdownLength }) return text;
        var sb = new StringBuilder();
        foreach (var span in ParseInline(text)) sb.Append(span.Text);
        return sb.ToString();
    }

    private static void Parse(string s, int start, int end, MdStyle style, string? link, List<MdSpan> output)
    {
        var buffer = new StringBuilder();

        // Once a scan for a closer from position p finds none, no later opener
        // can find one either (its candidates are a subset with the same
        // validity), so remember the failure instead of rescanning. This keeps
        // a run of unmatched `*`, `_`, backticks or `[` linear, not quadratic.
        var noCloserFrom = new Dictionary<string, int>();
        int noBacktickFrom = int.MaxValue;
        int noLinkFrom = int.MaxValue;

        void Flush()
        {
            if (buffer.Length == 0) return;
            output.Add(new MdSpan(buffer.ToString(), style, link));
            buffer.Clear();
        }

        int i = start;
        while (i < end)
        {
            char c = s[i];

            // Backslash escapes a markdown punctuation character.
            if (c == '\\' && i + 1 < end && IsEscapable(s[i + 1]))
            {
                buffer.Append(s[i + 1]);
                i += 2;
                continue;
            }

            // Fenced code block: ```[lang]\n...\n``` (or unterminated to EOF) — contents are literal, no nested parsing.
            if (c == '`' && i + 2 < end && s[i + 1] == '`' && s[i + 2] == '`')
            {
                int close = s.IndexOf("```", i + 3, end - (i + 3), StringComparison.Ordinal);
                bool terminated = close >= i + 3;
                int contentEnd = terminated ? close : end;
                int bodyStart = i + 3;
                int nl = s.IndexOf('\n', bodyStart, contentEnd - bodyStart);
                if (nl >= bodyStart)
                {
                    var firstLine = s.Substring(bodyStart, nl - bodyStart).TrimEnd('\r');
                    if (firstLine.Length == 0 || IsLanguageTag(firstLine))
                    {
                        bodyStart = nl + 1;
                    }
                }
                int bodyEnd = contentEnd;
                if (bodyEnd > bodyStart && s[bodyEnd - 1] == '\n')
                {
                    bodyEnd--;
                    if (bodyEnd > bodyStart && s[bodyEnd - 1] == '\r') bodyEnd--;
                }
                Flush();
                output.Add(new MdSpan(s.Substring(bodyStart, bodyEnd - bodyStart), style | MdStyle.Code, link));
                i = terminated ? close + 3 : end;
                continue;
            }

            // Inline code: contents are literal, no nested parsing.
            if (c == '`' && i + 1 < noBacktickFrom)
            {
                int close = s.IndexOf('`', i + 1, end - i - 1);
                if (close > i + 1)
                {
                    Flush();
                    output.Add(new MdSpan(s.Substring(i + 1, close - i - 1), style | MdStyle.Code, link));
                    i = close + 1;
                    continue;
                }
                if (close < 0) noBacktickFrom = i + 1;
            }

            // [label](target) — not inside another link.
            if (c == '[' && link == null && i < noLinkFrom)
            {
                if (TryLink(s, i, end, out var labelEnd, out var target, out var after))
                {
                    Flush();
                    Parse(s, i + 1, labelEnd, style, target, output);
                    i = after;
                    continue;
                }
                // No "](" ahead at all means no later '[' can be a link either.
                if (s.IndexOf("](", i + 1, end - i - 1, StringComparison.Ordinal) < 0) noLinkFrom = i;
            }

            // **bold** / __bold__
            if ((c == '*' || c == '_') && i + 1 < end && s[i + 1] == c &&
                TryDelimited(s, i, end, new string(c, 2), noCloserFrom, out var innerEnd))
            {
                Flush();
                Parse(s, i + 2, innerEnd, style | MdStyle.Bold, link, output);
                i = innerEnd + 2;
                continue;
            }

            // *italic* / _italic_
            if ((c == '*' || c == '_') && TryDelimited(s, i, end, c.ToString(), noCloserFrom, out var italicEnd))
            {
                Flush();
                Parse(s, i + 1, italicEnd, style | MdStyle.Italic, link, output);
                i = italicEnd + 1;
                continue;
            }

            buffer.Append(c);
            i++;
        }

        Flush();
    }

    /// <summary>
    /// Find the closing delimiter for an emphasis run opening at <paramref name="open"/>.
    /// The opener must be followed by non-space and the closer preceded by
    /// non-space. Underscores additionally must sit on word boundaries, so
    /// snake_case_names are left alone.
    /// </summary>
    private static bool TryDelimited(string s, int open, int end, string delim, Dictionary<string, int> noCloserFrom, out int close)
    {
        close = -1;
        int contentStart = open + delim.Length;
        if (contentStart >= end || char.IsWhiteSpace(s[contentStart])) return false;

        bool underscore = delim[0] == '_';
        if (underscore && open > 0 && char.IsLetterOrDigit(s[open - 1])) return false;

        if (noCloserFrom.TryGetValue(delim, out var failedFrom) && contentStart >= failedFrom) return false;

        int search = contentStart;
        while (search < end)
        {
            int candidate = s.IndexOf(delim, search, end - search, StringComparison.Ordinal);
            if (candidate < 0) break;

            // In a longer run (`***`), close on its last delimiters so the
            // leftover character belongs to the nested span: `**a *b***`.
            if (delim.Length == 2)
            {
                while (candidate + delim.Length < end && s[candidate + delim.Length] == delim[0]) candidate++;
            }

            // A single delimiter must not be half of a double one.
            bool partOfLonger = delim.Length == 1 &&
                                ((candidate + 1 < end && s[candidate + 1] == delim[0]) ||
                                 (candidate > contentStart && s[candidate - 1] == delim[0]));

            bool closesCleanly = candidate > contentStart &&
                                 !char.IsWhiteSpace(s[candidate - 1]) &&
                                 !partOfLonger;

            if (closesCleanly && underscore)
            {
                int afterClose = candidate + delim.Length;
                closesCleanly = afterClose >= s.Length || !char.IsLetterOrDigit(s[afterClose]);
            }

            if (closesCleanly)
            {
                close = candidate;
                return true;
            }

            search = candidate + delim.Length;
        }

        noCloserFrom[delim] = noCloserFrom.TryGetValue(delim, out var prior) ? Math.Min(prior, contentStart) : contentStart;
        return false;
    }

    private static bool TryLink(string s, int open, int end, out int labelEnd, out string target, out int after)
    {
        labelEnd = -1;
        target = "";
        after = -1;

        int close = s.IndexOf("](", open + 1, end - open - 1, StringComparison.Ordinal);
        if (close <= open + 1) return false;

        int targetEnd = s.IndexOf(')', close + 2, end - close - 2);
        if (targetEnd < 0) return false;

        var candidate = s.Substring(close + 2, targetEnd - close - 2).Trim();
        if (candidate.Length == 0 || candidate.Contains(' ')) return false;

        labelEnd = close;
        target = candidate;
        after = targetEnd + 1;
        return true;
    }

    private static bool IsLanguageTag(string token)
    {
        if (token.Length == 0) return false;
        foreach (char c in token)
        {
            if (!char.IsLetterOrDigit(c) && c != '_') return false;
        }
        return true;
    }

    private static bool IsEscapable(char c) => c is '\\' or '*' or '_' or '`' or '[' or ']' or '(' or ')' or '>' or '-' or '#';

    private static List<MdSpan> Merge(List<MdSpan> spans)
    {
        var merged = new List<MdSpan>(spans.Count);
        foreach (var span in spans)
        {
            if (span.Text.Length == 0) continue;
            if (merged.Count > 0 && merged[^1].Style == span.Style && merged[^1].Link == span.Link)
            {
                merged[^1] = merged[^1] with { Text = merged[^1].Text + span.Text };
            }
            else
            {
                merged.Add(span);
            }
        }
        return merged;
    }
}

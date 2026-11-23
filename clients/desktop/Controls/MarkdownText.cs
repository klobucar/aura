using System;
using System.Collections.Generic;
using System.Linq;
using Aura.Desktop.Services;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Controls.Documents;
using Avalonia.Input;
using Avalonia.Layout;
using Avalonia.Media;

namespace Aura.Desktop.Controls;

/// <summary>
/// A TextBlock that renders the handoff's inline markdown subset (bold,
/// italic, inline code, links) from <see cref="Markdown"/>. One renderer for
/// chat, profile comments and channel MOTDs, as the handoff asks.
///
/// Links are followed only for http(s), only on an explicit click, and never
/// prefetched — no remote content is ever loaded to render a message.
/// </summary>
public class MarkdownText : TextBlock
{
    public static readonly StyledProperty<string?> MarkdownProperty =
        AvaloniaProperty.Register<MarkdownText, string?>(nameof(Markdown));

    public static readonly StyledProperty<IBrush?> LinkBrushProperty =
        AvaloniaProperty.Register<MarkdownText, IBrush?>(nameof(LinkBrush));

    public static readonly StyledProperty<IBrush?> CodeBackgroundProperty =
        AvaloniaProperty.Register<MarkdownText, IBrush?>(nameof(CodeBackground));

    public static readonly StyledProperty<FontFamily?> CodeFontFamilyProperty =
        AvaloniaProperty.Register<MarkdownText, FontFamily?>(nameof(CodeFontFamily));

    /// <summary>Our display name: `@name` tokens for it render as caution pills.</summary>
    public static readonly StyledProperty<string?> MentionNameProperty =
        AvaloniaProperty.Register<MarkdownText, string?>(nameof(MentionName));

    public static readonly StyledProperty<IBrush?> MentionBackgroundProperty =
        AvaloniaProperty.Register<MarkdownText, IBrush?>(nameof(MentionBackground));

    public static readonly StyledProperty<IBrush?> MentionForegroundProperty =
        AvaloniaProperty.Register<MarkdownText, IBrush?>(nameof(MentionForeground));

    /// <summary>Spans from the parse that carry an openable link, by character range.</summary>
    private readonly List<(int Start, int End, Uri Target)> _links = new();

    protected override Type StyleKeyOverride => typeof(TextBlock);

    public string? Markdown
    {
        get => GetValue(MarkdownProperty);
        set => SetValue(MarkdownProperty, value);
    }

    public IBrush? LinkBrush
    {
        get => GetValue(LinkBrushProperty);
        set => SetValue(LinkBrushProperty, value);
    }

    public IBrush? CodeBackground
    {
        get => GetValue(CodeBackgroundProperty);
        set => SetValue(CodeBackgroundProperty, value);
    }

    public FontFamily? CodeFontFamily
    {
        get => GetValue(CodeFontFamilyProperty);
        set => SetValue(CodeFontFamilyProperty, value);
    }

    public string? MentionName
    {
        get => GetValue(MentionNameProperty);
        set => SetValue(MentionNameProperty, value);
    }

    public IBrush? MentionBackground
    {
        get => GetValue(MentionBackgroundProperty);
        set => SetValue(MentionBackgroundProperty, value);
    }

    public IBrush? MentionForeground
    {
        get => GetValue(MentionForegroundProperty);
        set => SetValue(MentionForegroundProperty, value);
    }

    protected override void OnPropertyChanged(AvaloniaPropertyChangedEventArgs change)
    {
        base.OnPropertyChanged(change);
        if (change.Property == MarkdownProperty ||
            change.Property == LinkBrushProperty ||
            change.Property == CodeBackgroundProperty ||
            change.Property == CodeFontFamilyProperty ||
            change.Property == MentionNameProperty ||
            change.Property == MentionBackgroundProperty ||
            change.Property == MentionForegroundProperty)
        {
            Rebuild();
        }
    }

    private void Rebuild()
    {
        _links.Clear();
        var inlines = new InlineCollection();
        int offset = 0;

        var spans = InlineMarkdown.ParseInline(Markdown);
        var plain = string.Concat(spans.Select(s => s.Text));
        var mentions = Mentions.FindNameMentions(plain, MentionName);

        foreach (var whole in spans)
        {
          foreach (var (span, isMention) in SplitAtMentions(whole, offset, mentions))
          {
            var run = new Run(span.Text);
            if (isMention)
            {
                if (MentionBackground != null) run.Background = MentionBackground;
                if (MentionForeground != null) run.Foreground = MentionForeground;
                run.FontWeight = FontWeight.SemiBold;
            }
            if (span.Style.HasFlag(MdStyle.Bold)) run.FontWeight = FontWeight.Bold;
            if (span.Style.HasFlag(MdStyle.Italic)) run.FontStyle = FontStyle.Italic;
            if (span.Style.HasFlag(MdStyle.Code))
            {
                if (CodeFontFamily != null) run.FontFamily = CodeFontFamily;
                if (CodeBackground != null) run.Background = CodeBackground;
            }
            if (span.Link != null)
            {
                if (LinkBrush != null) run.Foreground = LinkBrush;
                run.TextDecorations = Avalonia.Media.TextDecorations.Underline;
                if (span.IsOpenableLink && Uri.TryCreate(span.Link, UriKind.Absolute, out var uri))
                {
                    _links.Add((offset, offset + span.Text.Length, uri));
                }
            }

            inlines.Add(run);
            offset += span.Text.Length;
          }
        }

        Inlines = inlines;
    }

    /// <summary>Cut a span where @mention ranges (in whole-text offsets) start and end.</summary>
    private static IEnumerable<(MdSpan Span, bool IsMention)> SplitAtMentions(
        MdSpan span, int spanStart, IReadOnlyList<(int Start, int Length)> mentions)
    {
        if (mentions.Count == 0)
        {
            yield return (span, false);
            yield break;
        }

        var spanEnd = spanStart + span.Text.Length;
        var cursor = spanStart;
        foreach (var (start, length) in mentions)
        {
            var mStart = Math.Max(start, spanStart);
            var mEnd = Math.Min(start + length, spanEnd);
            if (mStart >= mEnd) continue;
            if (mStart > cursor) yield return (span with { Text = span.Text[(cursor - spanStart)..(mStart - spanStart)] }, false);
            yield return (span with { Text = span.Text[(mStart - spanStart)..(mEnd - spanStart)] }, true);
            cursor = mEnd;
        }
        if (cursor < spanEnd) yield return (span with { Text = span.Text[(cursor - spanStart)..] }, false);
    }

    private Uri? LinkAt(Point point)
    {
        if (_links.Count == 0) return null;

        var padding = Padding;
        var local = new Point(point.X - padding.Left, point.Y - padding.Top);
        var hit = TextLayout.HitTestPoint(local);
        if (!hit.IsInside) return null;

        var index = hit.TextPosition;
        foreach (var (start, end, target) in _links)
        {
            if (index >= start && index < end) return target;
        }
        return null;
    }

    protected override void OnPointerMoved(PointerEventArgs e)
    {
        base.OnPointerMoved(e);
        if (_links.Count == 0) return;
        Cursor = LinkAt(e.GetPosition(this)) != null ? new Cursor(StandardCursorType.Hand) : Cursor.Default;
    }

    protected override void OnPointerReleased(PointerReleasedEventArgs e)
    {
        base.OnPointerReleased(e);
        if (e.InitialPressMouseButton != MouseButton.Left) return;

        var target = LinkAt(e.GetPosition(this));
        if (target == null) return;

        e.Handled = true;
        _ = TopLevel.GetTopLevel(this)?.Launcher.LaunchUriAsync(target);
    }
}

/// <summary>
/// Block-level markdown for comment cards (§6): paragraphs, bullets and quotes,
/// each rendered with <see cref="MarkdownText"/>.
/// </summary>
public class MarkdownView : StackPanel
{
    public static readonly StyledProperty<string?> MarkdownProperty =
        AvaloniaProperty.Register<MarkdownView, string?>(nameof(Markdown));

    public static readonly StyledProperty<IBrush?> LinkBrushProperty =
        AvaloniaProperty.Register<MarkdownView, IBrush?>(nameof(LinkBrush));

    public static readonly StyledProperty<IBrush?> AccentBrushProperty =
        AvaloniaProperty.Register<MarkdownView, IBrush?>(nameof(AccentBrush));

    public static readonly StyledProperty<IBrush?> CodeBackgroundProperty =
        AvaloniaProperty.Register<MarkdownView, IBrush?>(nameof(CodeBackground));

    public static readonly StyledProperty<FontFamily?> CodeFontFamilyProperty =
        AvaloniaProperty.Register<MarkdownView, FontFamily?>(nameof(CodeFontFamily));

    public static readonly StyledProperty<IBrush?> TextBrushProperty =
        AvaloniaProperty.Register<MarkdownView, IBrush?>(nameof(TextBrush));

    public static readonly StyledProperty<double> TextSizeProperty =
        AvaloniaProperty.Register<MarkdownView, double>(nameof(TextSize), 12);

    protected override Type StyleKeyOverride => typeof(StackPanel);

    public string? Markdown { get => GetValue(MarkdownProperty); set => SetValue(MarkdownProperty, value); }
    public IBrush? LinkBrush { get => GetValue(LinkBrushProperty); set => SetValue(LinkBrushProperty, value); }
    public IBrush? AccentBrush { get => GetValue(AccentBrushProperty); set => SetValue(AccentBrushProperty, value); }
    public IBrush? CodeBackground { get => GetValue(CodeBackgroundProperty); set => SetValue(CodeBackgroundProperty, value); }
    public FontFamily? CodeFontFamily { get => GetValue(CodeFontFamilyProperty); set => SetValue(CodeFontFamilyProperty, value); }
    public IBrush? TextBrush { get => GetValue(TextBrushProperty); set => SetValue(TextBrushProperty, value); }
    public double TextSize { get => GetValue(TextSizeProperty); set => SetValue(TextSizeProperty, value); }

    public MarkdownView()
    {
        Spacing = 4;
    }

    protected override void OnPropertyChanged(AvaloniaPropertyChangedEventArgs change)
    {
        base.OnPropertyChanged(change);
        if (change.Property == MarkdownProperty || change.Property == LinkBrushProperty ||
            change.Property == AccentBrushProperty || change.Property == CodeBackgroundProperty ||
            change.Property == CodeFontFamilyProperty || change.Property == TextBrushProperty ||
            change.Property == TextSizeProperty)
        {
            Rebuild();
        }
    }

    private MarkdownText Line(string markdown) => new()
    {
        Markdown = markdown,
        LinkBrush = LinkBrush,
        CodeBackground = CodeBackground,
        CodeFontFamily = CodeFontFamily,
        Foreground = TextBrush,
        FontSize = TextSize,
        LineHeight = TextSize * 1.55,
        TextWrapping = TextWrapping.Wrap,
    };

    private void Rebuild()
    {
        Children.Clear();
        if (string.IsNullOrWhiteSpace(Markdown)) return;

        // Re-joining each block's own source keeps the inline parse identical
        // to the chat renderer's.
        foreach (var raw in Markdown.Replace("\r\n", "\n").Split('\n'))
        {
            var trimmed = raw.Trim();
            if (trimmed.Length == 0) continue;

            if (trimmed.StartsWith("- ") || trimmed.StartsWith("* "))
            {
                var row = new DockPanel();
                var dot = new Border
                {
                    Width = 4,
                    Height = 4,
                    CornerRadius = new CornerRadius(2),
                    Background = AccentBrush,
                    Margin = new Thickness(2, TextSize * 0.6, 8, 0),
                    VerticalAlignment = VerticalAlignment.Top,
                };
                DockPanel.SetDock(dot, Dock.Left);
                row.Children.Add(dot);
                row.Children.Add(Line(trimmed[2..]));
                Children.Add(row);
            }
            else if (trimmed.StartsWith(">"))
            {
                Children.Add(new Border
                {
                    BorderBrush = AccentBrush,
                    BorderThickness = new Thickness(3, 0, 0, 0),
                    Padding = new Thickness(9, 1, 0, 1),
                    Child = Line(trimmed[1..].TrimStart()),
                });
            }
            else
            {
                Children.Add(Line(trimmed));
            }
        }
    }
}

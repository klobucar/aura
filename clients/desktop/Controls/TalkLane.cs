using System.Collections.Generic;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Media;
using Aura.Desktop.Services;

namespace Aura.Desktop.Controls;

/// <summary>
/// One participant's talk lane: a well-inset track with the rolling window's
/// speech segments laid over it, plus the amber "talked over" overlaps drawn on
/// top. Fractions are window-relative (0 = −3 min, 1 = now) so the lane reflows
/// on resize without the view model knowing any pixel widths.
///
/// This is the Avalonia translation of the macOS ZStack overlay — a single
/// custom-drawn control rather than absolutely positioned children, which keeps
/// the per-second lane rebuild allocation-free in the visual tree.
/// </summary>
public class TalkLane : Control
{
    public static readonly StyledProperty<IReadOnlyList<LaneSlice>?> SegmentsProperty =
        AvaloniaProperty.Register<TalkLane, IReadOnlyList<LaneSlice>?>(nameof(Segments));

    public static readonly StyledProperty<IReadOnlyList<LaneSlice>?> OverlapsProperty =
        AvaloniaProperty.Register<TalkLane, IReadOnlyList<LaneSlice>?>(nameof(Overlaps));

    public static readonly StyledProperty<IBrush?> TrackBrushProperty =
        AvaloniaProperty.Register<TalkLane, IBrush?>(nameof(TrackBrush));

    public static readonly StyledProperty<IBrush?> TrackBorderBrushProperty =
        AvaloniaProperty.Register<TalkLane, IBrush?>(nameof(TrackBorderBrush));

    public static readonly StyledProperty<IBrush?> SegmentBrushProperty =
        AvaloniaProperty.Register<TalkLane, IBrush?>(nameof(SegmentBrush));

    public static readonly StyledProperty<IBrush?> LiveSegmentBrushProperty =
        AvaloniaProperty.Register<TalkLane, IBrush?>(nameof(LiveSegmentBrush));

    public static readonly StyledProperty<IBrush?> OverlapBrushProperty =
        AvaloniaProperty.Register<TalkLane, IBrush?>(nameof(OverlapBrush));

    /// <summary>8px in the rail, 12px on the stage.</summary>
    public static readonly StyledProperty<double> SegmentHeightProperty =
        AvaloniaProperty.Register<TalkLane, double>(nameof(SegmentHeight), 8);

    /// <summary>4 in the rail, 6 on the stage — the desktop badge/control radii.</summary>
    public static readonly StyledProperty<double> SegmentRadiusProperty =
        AvaloniaProperty.Register<TalkLane, double>(nameof(SegmentRadius), 4);

    /// <summary>Track corner radius: 6, the desktop control radius.</summary>
    public static readonly StyledProperty<double> TrackRadiusProperty =
        AvaloniaProperty.Register<TalkLane, double>(nameof(TrackRadius), 6);

    static TalkLane()
    {
        AffectsRender<TalkLane>(
            SegmentsProperty, OverlapsProperty, TrackBrushProperty, TrackBorderBrushProperty,
            SegmentBrushProperty, LiveSegmentBrushProperty, OverlapBrushProperty,
            SegmentHeightProperty, SegmentRadiusProperty, TrackRadiusProperty);
    }

    public IReadOnlyList<LaneSlice>? Segments
    {
        get => GetValue(SegmentsProperty);
        set => SetValue(SegmentsProperty, value);
    }

    public IReadOnlyList<LaneSlice>? Overlaps
    {
        get => GetValue(OverlapsProperty);
        set => SetValue(OverlapsProperty, value);
    }

    public IBrush? TrackBrush
    {
        get => GetValue(TrackBrushProperty);
        set => SetValue(TrackBrushProperty, value);
    }

    public IBrush? TrackBorderBrush
    {
        get => GetValue(TrackBorderBrushProperty);
        set => SetValue(TrackBorderBrushProperty, value);
    }

    public IBrush? SegmentBrush
    {
        get => GetValue(SegmentBrushProperty);
        set => SetValue(SegmentBrushProperty, value);
    }

    public IBrush? LiveSegmentBrush
    {
        get => GetValue(LiveSegmentBrushProperty);
        set => SetValue(LiveSegmentBrushProperty, value);
    }

    public IBrush? OverlapBrush
    {
        get => GetValue(OverlapBrushProperty);
        set => SetValue(OverlapBrushProperty, value);
    }

    public double SegmentHeight
    {
        get => GetValue(SegmentHeightProperty);
        set => SetValue(SegmentHeightProperty, value);
    }

    public double SegmentRadius
    {
        get => GetValue(SegmentRadiusProperty);
        set => SetValue(SegmentRadiusProperty, value);
    }

    public double TrackRadius
    {
        get => GetValue(TrackRadiusProperty);
        set => SetValue(TrackRadiusProperty, value);
    }

    public override void Render(DrawingContext context)
    {
        var width = Bounds.Width;
        var height = Bounds.Height;
        if (width <= 0 || height <= 0) return;

        // Track: white @ 5% fill, 1px white @ 8% rim.
        var track = new Rect(0, 0, width, height);
        var pen = TrackBorderBrush is null ? null : new Pen(TrackBorderBrush, 1);
        context.DrawRectangle(TrackBrush, pen, new RoundedRect(track, TrackRadius));

        var segmentHeight = System.Math.Min(SegmentHeight, height);
        var top = (height - segmentHeight) / 2;

        DrawSlices(context, Segments, width, top, segmentHeight, live: true);
        // Overlaps last: the "talked over" marker sits on top of the speech.
        DrawSlices(context, Overlaps, width, top, segmentHeight, live: false, forceBrush: OverlapBrush);
    }

    private void DrawSlices(
        DrawingContext context,
        IReadOnlyList<LaneSlice>? slices,
        double width,
        double top,
        double segmentHeight,
        bool live,
        IBrush? forceBrush = null)
    {
        if (slices is null) return;

        foreach (var slice in slices)
        {
            var x = slice.Start * width;
            // Keep sub-pixel blips visible; a 200ms segment in a 3 minute window
            // is 0.4px wide and would otherwise vanish.
            var sliceWidth = System.Math.Max(slice.Width * width, 2);
            if (x + sliceWidth > width) x = System.Math.Max(0, width - sliceWidth);

            var brush = forceBrush ?? (live && slice.IsLive ? LiveSegmentBrush ?? SegmentBrush : SegmentBrush);
            if (brush is null) continue;

            var rect = new Rect(x, top, sliceWidth, segmentHeight);
            context.DrawRectangle(brush, null, new RoundedRect(rect, SegmentRadius));
        }
    }
}

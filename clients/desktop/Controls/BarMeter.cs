using System;
using System.Collections.Generic;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Media;

namespace Aura.Desktop.Controls;

public enum BarMeterMode
{
    /// <summary>Live waveform: one bar per recent level sample, height and
    /// opacity by level, centred on the midline.</summary>
    Waveform,

    /// <summary>Input meter: fixed staircase of bars, lit up to the level.</summary>
    Level,
}

/// <summary>
/// The rail's two bar readouts — the speaker card's 16-bar live waveform and the
/// HUD's 9-bar input meter. One custom-drawn control instead of an ItemsRepeater
/// of bar view models: these repaint several times a second and carry no state
/// worth binding per bar.
/// </summary>
public class BarMeter : Control
{
    public static readonly StyledProperty<IReadOnlyList<double>?> LevelsProperty =
        AvaloniaProperty.Register<BarMeter, IReadOnlyList<double>?>(nameof(Levels));

    public static readonly StyledProperty<BarMeterMode> ModeProperty =
        AvaloniaProperty.Register<BarMeter, BarMeterMode>(nameof(Mode), BarMeterMode.Waveform);

    /// <summary>Overall level 0..1, used by <see cref="BarMeterMode.Level"/>.</summary>
    public static readonly StyledProperty<double> LevelProperty =
        AvaloniaProperty.Register<BarMeter, double>(nameof(Level));

    /// <summary>16 in the rail, 26 on the stage; 9 for the input meter.</summary>
    public static readonly StyledProperty<int> BarCountProperty =
        AvaloniaProperty.Register<BarMeter, int>(nameof(BarCount), 16);

    public static readonly StyledProperty<double> BarGapProperty =
        AvaloniaProperty.Register<BarMeter, double>(nameof(BarGap), 2);

    public static readonly StyledProperty<double> BarRadiusProperty =
        AvaloniaProperty.Register<BarMeter, double>(nameof(BarRadius), 2);

    public static readonly StyledProperty<IBrush?> BarBrushProperty =
        AvaloniaProperty.Register<BarMeter, IBrush?>(nameof(BarBrush));

    /// <summary>Bars below the hot threshold, and unlit meter bars.</summary>
    public static readonly StyledProperty<IBrush?> DimBarBrushProperty =
        AvaloniaProperty.Register<BarMeter, IBrush?>(nameof(DimBarBrush));

    /// <summary>Level above which a bar switches to the full-strength brush.</summary>
    public static readonly StyledProperty<double> HotThresholdProperty =
        AvaloniaProperty.Register<BarMeter, double>(nameof(HotThreshold), 0.6);

    static BarMeter()
    {
        AffectsRender<BarMeter>(
            LevelsProperty, ModeProperty, LevelProperty, BarCountProperty, BarGapProperty,
            BarRadiusProperty, BarBrushProperty, DimBarBrushProperty, HotThresholdProperty);
    }

    public IReadOnlyList<double>? Levels
    {
        get => GetValue(LevelsProperty);
        set => SetValue(LevelsProperty, value);
    }

    public BarMeterMode Mode
    {
        get => GetValue(ModeProperty);
        set => SetValue(ModeProperty, value);
    }

    public double Level
    {
        get => GetValue(LevelProperty);
        set => SetValue(LevelProperty, value);
    }

    public int BarCount
    {
        get => GetValue(BarCountProperty);
        set => SetValue(BarCountProperty, value);
    }

    public double BarGap
    {
        get => GetValue(BarGapProperty);
        set => SetValue(BarGapProperty, value);
    }

    public double BarRadius
    {
        get => GetValue(BarRadiusProperty);
        set => SetValue(BarRadiusProperty, value);
    }

    public IBrush? BarBrush
    {
        get => GetValue(BarBrushProperty);
        set => SetValue(BarBrushProperty, value);
    }

    public IBrush? DimBarBrush
    {
        get => GetValue(DimBarBrushProperty);
        set => SetValue(DimBarBrushProperty, value);
    }

    public double HotThreshold
    {
        get => GetValue(HotThresholdProperty);
        set => SetValue(HotThresholdProperty, value);
    }

    public override void Render(DrawingContext context)
    {
        var count = Math.Max(1, BarCount);
        var width = Bounds.Width;
        var height = Bounds.Height;
        if (width <= 0 || height <= 0) return;

        var barWidth = (width - BarGap * (count - 1)) / count;
        if (barWidth <= 0) return;

        for (int i = 0; i < count; i++)
        {
            var x = i * (barWidth + BarGap);
            double level = Mode == BarMeterMode.Waveform ? SampleAt(i, count) : 1;
            bool lit = Mode != BarMeterMode.Level || (i + 1) / (double)count <= Level;

            double barHeight;
            if (Mode == BarMeterMode.Waveform)
            {
                // Never fully collapse a bar; a flat line still reads as a meter.
                barHeight = Math.Max(2, level * height);
            }
            else
            {
                // Staircase: the meter rises left to right so a glance reads level.
                barHeight = height * (0.35 + 0.65 * (i + 1) / count);
            }

            var y = (height - barHeight) / 2;
            var brush = Mode == BarMeterMode.Waveform
                ? (level >= HotThreshold ? BarBrush : DimBarBrush ?? BarBrush)
                : (lit ? BarBrush : DimBarBrush);
            if (brush is null) continue;

            var rect = new Rect(x, y, barWidth, barHeight);
            context.DrawRectangle(brush, null, new RoundedRect(rect, BarRadius));
        }
    }

    /// <summary>
    /// Newest sample on the right. Fewer samples than bars leaves the left end
    /// flat rather than stretching history.
    /// </summary>
    private double SampleAt(int index, int count)
    {
        var levels = Levels;
        if (levels is null || levels.Count == 0) return 0;

        var offset = count - levels.Count;
        var sampleIndex = index - offset;
        if (sampleIndex < 0) return 0;
        if (sampleIndex >= levels.Count) return 0;
        return Math.Clamp(levels[sampleIndex], 0, 1);
    }
}

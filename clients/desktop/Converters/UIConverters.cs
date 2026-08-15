using System;
using System.Globalization;
using Avalonia;
using Avalonia.Data.Converters;
using Avalonia.Layout;
using Avalonia.Media;
using Aura.Desktop.ViewModels;

namespace Aura.Desktop.Converters;

/// <summary>Converts a bool to Left/Right horizontal alignment for chat bubbles.</summary>
public class BoolToAlignmentConverter : IValueConverter
{
    public static readonly BoolToAlignmentConverter Instance = new();
    public static readonly BoolToAlignmentConverter CenterIfSystem = new() { IsSystemMessageConverter = true };

    public bool IsSystemMessageConverter { get; set; }

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is bool b)
        {
            if (IsSystemMessageConverter)
                return b ? HorizontalAlignment.Center : HorizontalAlignment.Stretch;
            return b ? HorizontalAlignment.Right : HorizontalAlignment.Left;
        }
        return HorizontalAlignment.Left;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Inverts a bool (for !IsConnected bindings).</summary>
public class InverseBoolConverter : IValueConverter
{
    public static readonly InverseBoolConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool b ? !b : false;

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool b ? !b : false;
}

/// <summary>Hides the name label for system messages.</summary>
public class BoolToOpacityConverter : IValueConverter
{
    public static readonly BoolToOpacityConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isSystem && isSystem ? 0.0 : 1.0;

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Chat bubble background based on sender/system.</summary>
public class ChatBubbleColorConverter : IValueConverter
{
    public static readonly ChatBubbleColorConverter Instance = new();

    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is ChatMessage msg)
        {
            if (msg.System) return Brushes.Transparent;
            // Outgoing: the primary -> secondary brand gradient. Incoming: glass.
            return TokenBrush.Get(msg.IsFromCurrentUser ? "AuraBrandGradientBrush" : "AuraPanelFillBrush");
        }
        return TokenBrush.Get("AuraPanelFillBrush");
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Chat bubble text color.</summary>
public class ChatBubbleTextColorConverter : IValueConverter
{
    public static readonly ChatBubbleTextColorConverter Instance = new();

    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is ChatMessage msg)
        {
            if (msg.System) return TokenBrush.Get("AuraTextDimBrush");
            if (msg.IsFromCurrentUser) return Brushes.White;
            return TokenBrush.Get("AuraTextBrush");
        }
        return TokenBrush.Get("AuraTextBrush");
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Border thickness: 1 for system messages, 0 for bubbles.</summary>
public class BoolToThicknessConverter : IValueConverter
{
    public static readonly BoolToThicknessConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isSystem && isSystem ? new Thickness(1) : new Thickness(0);

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Corner radius: smaller for system messages.</summary>
public class SystemMessageCornerRadiusConverter : IValueConverter
{
    public static readonly SystemMessageCornerRadiusConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isSystem && isSystem ? new CornerRadius(10) : new CornerRadius(18);

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Padding: tighter for system messages.</summary>
public class SystemMessagePaddingConverter : IValueConverter
{
    public static readonly SystemMessagePaddingConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isSystem && isSystem ? new Thickness(10, 4) : new Thickness(16, 10);

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Mic icon text for toggle button.</summary>
public class MicIconConverter : IValueConverter
{
    public static readonly MicIconConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isEnabled && isEnabled ? "🎤" : "🎙";

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Deafen icon text for toggle button.</summary>
public class DeafenIconConverter : IValueConverter
{
    public static readonly DeafenIconConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isDeafened && isDeafened ? "🔇" : "🎧";

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Speaking user name color: accent while speaking, textMid otherwise.</summary>
public class StatusToBrushConverter : IValueConverter
{
    public static readonly StatusToBrushConverter Instance = new();

    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => TokenBrush.Get(value is bool isSpeaking && isSpeaking ? "AuraAccentBrush" : "AuraTextMidBrush");

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Mic orb fill color: accent when active, the inset well when muted.</summary>
public class MicOrbBrushConverter : IValueConverter
{
    public static readonly MicOrbBrushConverter Instance = new();

    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => TokenBrush.Get(value is bool isEnabled && isEnabled ? "AuraAccentBrush" : "AuraWellBrush");

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Converts a non-null/non-empty string to true, null/empty to false. Use for IsVisible bindings.</summary>
public class StringToBoolConverter : IValueConverter
{
    public static readonly StringToBoolConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is string s && !string.IsNullOrEmpty(s);

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>
/// Resolves a design token brush by key from the active theme dictionary. Used by
/// the converters below so state colors come from the palette rather than literals.
/// </summary>
internal static class TokenBrush
{
    public static IBrush? Get(string key)
    {
        var app = Application.Current;
        if (app == null) return null;
        return app.Resources.TryGetResource(key, app.ActualThemeVariant, out var value) ? value as IBrush : null;
    }
}

/// <summary>Current channel name is 650 weight; the rest are regular.</summary>
public class CurrentChannelWeightConverter : IValueConverter
{
    public static readonly CurrentChannelWeightConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isCurrent && isCurrent ? FontWeight.SemiBold : FontWeight.Normal;

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Speaking member row: accent @ 9% fill, transparent otherwise.</summary>
public class SpeakingRowBrushConverter : IValueConverter
{
    public static readonly SpeakingRowBrushConverter Instance = new();

    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isSpeaking && isSpeaking ? TokenBrush.Get("AuraAccentRowFillBrush") : Brushes.Transparent;

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Avatar fill: brand gradient while active, the inset well when idle.</summary>
public class AvatarBrushConverter : IValueConverter
{
    public static readonly AvatarBrushConverter Instance = new();

    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => TokenBrush.Get(value is bool active && active ? "AuraBrandGradientBrush" : "AuraWellBrush");

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>
/// The silent list's trust row: `caution @ 12%` fill with a `caution @ 30%` border
/// and caution text when the key hasn't been compared; neutral otherwise.
/// </summary>
public class TrustRowBrushConverter : IValueConverter
{
    public static readonly TrustRowBrushConverter Fill = new() { TrueKey = "AuraCautionFillBrush" };
    public static readonly TrustRowBrushConverter Border = new() { TrueKey = "AuraCautionBorderBrush" };
    public static readonly TrustRowBrushConverter Text = new() { TrueKey = "AuraCautionTextBrush", FalseKey = "AuraWarnBrush" };

    public string TrueKey { get; set; } = "";
    public string? FalseKey { get; set; }

    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var needsTrust = value is bool b && b;
        if (needsTrust) return TokenBrush.Get(TrueKey);
        return FalseKey == null ? Brushes.Transparent : TokenBrush.Get(FalseKey);
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Connection dot: good when connected, textDim when not.</summary>
public class BoolToColorConverter : IValueConverter
{
    public static readonly BoolToColorConverter Instance = new();

    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => TokenBrush.Get(value is bool isConnected && isConnected ? "AuraGoodBrush" : "AuraTextDimBrush");

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

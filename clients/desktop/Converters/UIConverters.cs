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

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is ChatMessage msg)
        {
            if (msg.System) return Brush.Parse("Transparent");
            if (msg.IsFromCurrentUser) return Brush.Parse("#89B4FA");
            return Brush.Parse("#313244");
        }
        return Brush.Parse("#313244");
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Chat bubble text color.</summary>
public class ChatBubbleTextColorConverter : IValueConverter
{
    public static readonly ChatBubbleTextColorConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is ChatMessage msg)
        {
            if (msg.System) return Brush.Parse("#6C7086");
            if (msg.IsFromCurrentUser) return Brush.Parse("#1E1E2E");
            return Brush.Parse("#CDD6F4");
        }
        return Brush.Parse("#CDD6F4");
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

/// <summary>Speaking user name color: green accent when speaking, muted grey otherwise.</summary>
public class StatusToBrushConverter : IValueConverter
{
    public static readonly StatusToBrushConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isSpeaking && isSpeaking
            ? Brush.Parse("#A6E3A1")
            : Brush.Parse("#A6ADC8");

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

/// <summary>Mic orb fill color: accent green when active, subtle grey when muted.</summary>
public class MicOrbBrushConverter : IValueConverter
{
    public static readonly MicOrbBrushConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isEnabled && isEnabled
            ? Brush.Parse("#A6E3A1")
            : Brush.Parse("#45475A");

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

/// <summary>Placeholder required by App.axaml resource key. Converts bool speaking state to a brush color.</summary>
public class BoolToColorConverter : IValueConverter
{
    public static readonly BoolToColorConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => value is bool isSpeaking && isSpeaking
            ? Brush.Parse("#A6E3A1")
            : Brush.Parse("#A6ADC8");

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotImplementedException();
}

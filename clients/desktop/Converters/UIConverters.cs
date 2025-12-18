using System;
using System.Globalization;
using Avalonia;
using Avalonia.Data.Converters;
using Avalonia.Layout;
using Avalonia.Media;
using Aura.Desktop.ViewModels;

namespace Aura.Desktop.Converters;

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
            {
                // If it's a system message, return Center, else use normal logic (not applicable here but for clarity)
                return b ? HorizontalAlignment.Center : HorizontalAlignment.Stretch;
            }
            return b ? HorizontalAlignment.Right : HorizontalAlignment.Left;
        }
        return HorizontalAlignment.Left;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) => throw new NotImplementedException();
}

public class BoolToOpacityConverter : IValueConverter
{
    public static readonly BoolToOpacityConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is bool isSystem && isSystem) return 0.0;
        return 1.0;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) => throw new NotImplementedException();
}

public class ChatBubbleColorConverter : IValueConverter
{
    public static readonly ChatBubbleColorConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is ChatMessage msg)
        {
            if (msg.System) return Brush.Parse("#1E1E2E"); // Match background for system messages or slightly different
            if (msg.IsFromCurrentUser) return Brush.Parse("#89B4FA");
            return Brush.Parse("#313244");
        }
        return Brush.Parse("#313244");
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) => throw new NotImplementedException();
}

public class ChatBubbleTextColorConverter : IValueConverter
{
    public static readonly ChatBubbleTextColorConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        // Value is ChatMessage
        if (value is ChatMessage msg)
        {
            if (msg.System) return Brush.Parse("#A6ADC8");
            if (msg.IsFromCurrentUser) return Brush.Parse("#1E1E2E");
            return Brush.Parse("#CDD6F4");
        }
        return Brush.Parse("#CDD6F4");
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) => throw new NotImplementedException();
}

public class BoolToThicknessConverter : IValueConverter
{
    public static readonly BoolToThicknessConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is bool isSystem && isSystem) return new Thickness(1);
        return new Thickness(0);
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) => throw new NotImplementedException();
}

public class SystemMessageCornerRadiusConverter : IValueConverter
{
    public static readonly SystemMessageCornerRadiusConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is bool isSystem && isSystem) return new CornerRadius(12);
        return new CornerRadius(18);
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) => throw new NotImplementedException();
}

public class SystemMessagePaddingConverter : IValueConverter
{
    public static readonly SystemMessagePaddingConverter Instance = new();

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is bool isSystem && isSystem) return new Thickness(12, 6);
        return new Thickness(16, 10);
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) => throw new NotImplementedException();
}

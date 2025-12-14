using System;
using System.Globalization;
using Avalonia.Data.Converters;
using Avalonia.Media;

namespace Aura.Desktop.Converters;

public class BoolToColorConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        if (value is bool isMuted)
        {
            // Red for muted, Green for unmuted
            return isMuted ? Color.Parse("#E74C3C") : Color.Parse("#2ECC71");
        }
        return Color.Parse("#999999");
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        throw new NotImplementedException();
    }
}

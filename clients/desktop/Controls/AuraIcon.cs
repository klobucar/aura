using Avalonia;
using Avalonia.Controls;
using Avalonia.Controls.Documents;
using Avalonia.Controls.Shapes;
using Avalonia.Media;

namespace Aura.Desktop.Controls;

/// <summary>
/// A stroked 24×24 outline icon (see Themes/AuraIcons.axaml). The path is laid
/// out in its full 24×24 box and the box is scaled to the control's size, so
/// icons keep one optical size and line weight however much of the box they
/// fill. The stroke follows the inherited text foreground, so an icon inside a
/// button takes the button's colour, hover and disabled states included.
/// </summary>
public class AuraIcon : Viewbox
{
    public static readonly StyledProperty<Geometry?> DataProperty =
        AvaloniaProperty.Register<AuraIcon, Geometry?>(nameof(Data));

    public static readonly StyledProperty<IBrush?> ForegroundProperty =
        TextElement.ForegroundProperty.AddOwner<AuraIcon>();

    /// <summary>Stroke width in the 24×24 design space; Lucide's is 2.</summary>
    public static readonly StyledProperty<double> StrokeWidthProperty =
        AvaloniaProperty.Register<AuraIcon, double>(nameof(StrokeWidth), 2);

    public Geometry? Data
    {
        get => GetValue(DataProperty);
        set => SetValue(DataProperty, value);
    }

    public IBrush? Foreground
    {
        get => GetValue(ForegroundProperty);
        set => SetValue(ForegroundProperty, value);
    }

    public double StrokeWidth
    {
        get => GetValue(StrokeWidthProperty);
        set => SetValue(StrokeWidthProperty, value);
    }

    private readonly Path _path = new()
    {
        Width = 24,
        Height = 24,
        Stretch = Stretch.None,
        StrokeLineCap = PenLineCap.Round,
        StrokeJoin = PenLineJoin.Round,
    };

    public AuraIcon()
    {
        Stretch = Stretch.Uniform;
        Child = new Canvas { Width = 24, Height = 24, Children = { _path } };
        _path.StrokeThickness = StrokeWidth;
    }

    protected override void OnPropertyChanged(AvaloniaPropertyChangedEventArgs change)
    {
        base.OnPropertyChanged(change);
        if (change.Property == DataProperty) _path.Data = Data;
        else if (change.Property == ForegroundProperty) _path.Stroke = Foreground;
        else if (change.Property == StrokeWidthProperty) _path.StrokeThickness = StrokeWidth;
    }
}

using Aura.Desktop.ViewModels;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Markup.Xaml;
using Avalonia.Media;

namespace Aura.Desktop.Views;

public partial class MainWindow : Window
{
    public MainWindow()
    {
        InitializeComponent();
        ApplyAcrylicTint();

        // The voice column's flex width is computed in the view model, which needs
        // to know how much room there is.
        SizeChanged += (_, e) =>
        {
            if (DataContext is MainWindowViewModel vm) vm.WindowWidth = e.NewSize.Width;
        };

        // Seed it too: a window that opens at its startup size may never raise
        // SizeChanged, and the flex column would size off the default.
        Opened += (_, _) =>
        {
            if (DataContext is MainWindowViewModel vm) vm.WindowWidth = Bounds.Width;
        };
    }

    private void InitializeComponent()
    {
        AvaloniaXamlLoader.Load(this);
    }

    /// <summary>
    /// Tint the window material from the palette. Set in code because the acrylic
    /// material is not part of the visual tree and can't resolve DynamicResource.
    /// </summary>
    private void ApplyAcrylicTint()
    {
        var border = this.FindControl<ExperimentalAcrylicBorder>("AcrylicRoot");
        if (border == null) return;

        var app = Application.Current;
        var tint = Color.Parse("#0A0A0D");
        if (app != null &&
            app.Resources.TryGetResource("AuraBackgroundColor", app.ActualThemeVariant, out var value) &&
            value is Color themed)
        {
            tint = themed;
        }

        border.Material = new ExperimentalAcrylicMaterial
        {
            BackgroundSource = AcrylicBackgroundSource.Digger,
            TintColor = tint,
            TintOpacity = 1,
            MaterialOpacity = 0.72,
            FallbackColor = tint,
        };
    }

    /// <summary>
    /// Push to talk, Ctrl+Space. In-window only: the low-level keyboard hook the
    /// handoff asks for is separate work, so this does nothing when the window
    /// isn't focused.
    /// </summary>
    protected override void OnKeyDown(KeyEventArgs e)
    {
        if (IsPushToTalkGesture(e) && DataContext is MainWindowViewModel vm)
        {
            e.Handled = true;
            _ = vm.BeginPushToTalkAsync();
            return;
        }

        base.OnKeyDown(e);
    }

    protected override void OnKeyUp(KeyEventArgs e)
    {
        if (e.Key == Key.Space && DataContext is MainWindowViewModel vm && vm.IsPushToTalkHeld)
        {
            e.Handled = true;
            _ = vm.EndPushToTalkAsync();
            return;
        }

        base.OnKeyUp(e);
    }

    private static bool IsPushToTalkGesture(KeyEventArgs e) =>
        e.Key == Key.Space && e.KeyModifiers.HasFlag(KeyModifiers.Control);
}

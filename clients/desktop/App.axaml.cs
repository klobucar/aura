using System;
using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Markup.Xaml;
using Aura.Desktop.Services;
using Aura.Desktop.ViewModels;
using Aura.Desktop.Views;
using uniffi.aura_core;

namespace Aura.Desktop;

public partial class App : Application
{
    public override void Initialize()
    {
        AvaloniaXamlLoader.Load(this);
    }

    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
        {
            StartTelemetry(desktop);
            desktop.MainWindow = new MainWindow
            {
                DataContext = new MainWindowViewModel(),
            };
        }

        base.OnFrameworkInitializationCompleted();
    }

    /// <summary>
    /// Opt-in OpenTelemetry tracing: only when OTEL_EXPORTER_OTLP_ENDPOINT is
    /// set (see docs/OBSERVABILITY.md). Exports operation names and outcomes,
    /// never identities or content.
    /// </summary>
    private static void StartTelemetry(IClassicDesktopStyleApplicationLifetime desktop)
    {
        try
        {
            NativeLibraryLoader.Initialize();
            if (AuraCoreMethods.TelemetryStartFromEnv("aura-desktop"))
            {
                desktop.Exit += (_, _) => AuraCoreMethods.TelemetryShutdown();
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[Telemetry] Not started: {ex.Message}");
        }
    }
}

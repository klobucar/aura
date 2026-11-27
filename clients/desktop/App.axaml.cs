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
            AuraNetworkClient.CleanupDownloadsCache();
            desktop.Exit += (_, _) => AuraNetworkClient.CleanupDownloadsCache();

            // Clicking an aura:// link launches us with the link as the first
            // argument. Registration is per-user and Windows-only; a link
            // opened while Aura is already running starts a second instance
            // (forwarding to the running one is not implemented).
            UrlSchemeRegistration.EnsureRegistered();

            var viewModel = new MainWindowViewModel();
            if (desktop.Args is { Length: > 0 } args && AuraInviteLink.LooksLikeInvite(args[0]))
            {
                viewModel.ApplyInviteLink(args[0]);
            }

            desktop.MainWindow = new MainWindow
            {
                DataContext = viewModel,
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

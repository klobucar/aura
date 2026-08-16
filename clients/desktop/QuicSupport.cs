using System;
using System.Net.Quic;
using System.Runtime.InteropServices;

namespace Aura.Desktop;

/// <summary>
/// Whether QUIC can run here, and if not, what the user can actually do about
/// it on <em>their</em> platform.
/// </summary>
/// <param name="IsSupported">QUIC is usable.</param>
/// <param name="Headline">One line for the status bar.</param>
/// <param name="Remedy">The concrete fix, or null when supported.</param>
public sealed record QuicAvailability(bool IsSupported, string Headline, string? Remedy);

/// <summary>
/// Aura speaks QUIC and nothing else, so an unsupported runtime is not a
/// degraded mode — it is the whole app not working.
///
/// The two platforms this client actually ships to fail for unrelated reasons
/// and need unrelated fixes: Linux is missing a package, Windows 10 is missing
/// OS crypto. Reporting either as a generic connection failure sends users
/// hunting for a bug in Aura instead of doing the one thing that would fix it.
/// </summary>
public static class QuicSupport
{
    /// <summary>Evaluated once — nothing here changes while the app runs.</summary>
    public static QuicAvailability Availability { get; } = Evaluate();

    private static QuicAvailability Evaluate()
    {
        if (QuicConnection.IsSupported)
        {
            return new QuicAvailability(true, "QUIC available", null);
        }

        if (RuntimeInformation.IsOSPlatform(OSPlatform.Linux))
        {
            // .NET ships msquic on Windows but not on Linux, where it is a
            // separate package built against OpenSSL.
            return new QuicAvailability(
                false,
                "QUIC unavailable — libmsquic is not installed",
                "Aura needs libmsquic 2.2 or newer. Install it with your package "
                + "manager (apt-get / dnf / zypper / apk install libmsquic). On most "
                + "distributions this comes from Microsoft's package repository; see "
                + "https://learn.microsoft.com/linux/packages");
        }

        if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
        {
            // The bundled msquic is Schannel-backed, and Schannel only gained
            // the TLS 1.3 APIs QUIC requires in Windows 11 / Server 2022.
            return new QuicAvailability(
                false,
                $"QUIC unavailable — this Windows build ({Environment.OSVersion.Version}) is too old",
                "QUIC requires Windows 11 or Windows Server 2022 or later. Earlier "
                + "versions lack the TLS 1.3 support it depends on, so Aura cannot "
                + "connect at all on this machine.");
        }

        if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
        {
            // Dev-loop only: macOS users run the native Swift client.
            return new QuicAvailability(
                false,
                "QUIC unavailable — libmsquic not found",
                "Install it with `brew install libmsquic`. If it is already "
                + "installed, launch with "
                + "DYLD_FALLBACK_LIBRARY_PATH=$(brew --prefix)/lib so the runtime "
                + "can find it.");
        }

        return new QuicAvailability(
            false,
            "QUIC unavailable on this platform",
            "Aura requires QUIC (msquic) and cannot connect without it.");
    }

    /// <summary>Diagnostics for the log; the UI reads <see cref="Availability"/>.</summary>
    public static void CheckQuicSupport()
    {
        var status = Availability;

        Console.WriteLine("[QuicSupport] Checking QUIC availability...");
        Console.WriteLine($"[QuicSupport] QuicConnection.IsSupported: {QuicConnection.IsSupported}");
        Console.WriteLine($"[QuicSupport] QuicListener.IsSupported: {QuicListener.IsSupported}");
        Console.WriteLine($"[QuicSupport] Runtime: {RuntimeInformation.RuntimeIdentifier}");
        Console.WriteLine($"[QuicSupport] OS: {Environment.OSVersion}");

        if (status.IsSupported)
        {
            Console.WriteLine("[QuicSupport] ✓ QUIC is supported");
            return;
        }

        Console.WriteLine($"[QuicSupport] ❌ {status.Headline}");
        Console.WriteLine($"[QuicSupport]    {status.Remedy}");
    }
}

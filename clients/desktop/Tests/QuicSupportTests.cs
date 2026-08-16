using System.Net.Quic;
using System.Runtime.InteropServices;
using Aura.Desktop;
using Xunit;

namespace Aura.Desktop.Tests;

/// <summary>
/// Aura speaks QUIC and nothing else, so an unsupported runtime is the whole
/// app failing, not a degraded mode. The two platforms this client ships to
/// fail for unrelated reasons and need unrelated fixes — a missing package on
/// Linux, an OS floor on Windows — so the remedy has to be specific or it is
/// not a remedy.
/// </summary>
public class QuicSupportTests
{
    [Fact]
    public void Availability_AgreesWithTheRuntime()
    {
        Assert.Equal(QuicConnection.IsSupported, QuicSupport.Availability.IsSupported);
    }

    [Fact]
    public void Availability_AlwaysCarriesAHeadline()
    {
        // The status line binds to this unconditionally.
        Assert.False(string.IsNullOrWhiteSpace(QuicSupport.Availability.Headline));
    }

    [Fact]
    public void Unsupported_CarriesARemedy_SupportedDoesNot()
    {
        var status = QuicSupport.Availability;

        if (status.IsSupported)
        {
            Assert.Null(status.Remedy);
        }
        else
        {
            // A banner that says "QUIC unavailable" and stops is what we are
            // replacing; the actionable half is the point.
            Assert.False(string.IsNullOrWhiteSpace(status.Remedy));
        }
    }

    [Fact]
    public void Unsupported_RemedyNamesSomethingTheUserCanDo()
    {
        var status = QuicSupport.Availability;
        if (status.IsSupported) return;   // nothing to assert on a healthy box

        var remedy = status.Remedy!;

        if (RuntimeInformation.IsOSPlatform(OSPlatform.Linux))
        {
            // .NET ships msquic on Windows but not Linux, where it is a package.
            Assert.Contains("libmsquic", remedy);
        }
        else if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
        {
            // Schannel only gained TLS 1.3 in Windows 11 / Server 2022.
            Assert.Contains("Windows 11", remedy);
        }
        else if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
        {
            Assert.Contains("libmsquic", remedy);
        }
    }
}

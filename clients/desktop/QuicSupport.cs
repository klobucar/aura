using System;
using System.Net.Quic;

namespace Aura.Desktop;

public static class QuicSupport
{
    public static void CheckQuicSupport()
    {
        Console.WriteLine("[QuicSupport] Checking QUIC availability...");
        Console.WriteLine($"[QuicSupport] QuicConnection.IsSupported: {QuicConnection.IsSupported}");
        Console.WriteLine($"[QuicSupport] QuicListener.IsSupported: {QuicListener.IsSupported}");
        Console.WriteLine($"[QuicSupport] Runtime: {System.Runtime.InteropServices.RuntimeInformation.RuntimeIdentifier}");
        Console.WriteLine($"[QuicSupport] OS: {Environment.OSVersion}");
        
        if (!QuicConnection.IsSupported)
        {
            Console.WriteLine("[QuicSupport] ❌ QUIC is NOT supported on this platform!");
            Console.WriteLine("[QuicSupport] This may be due to:");
            Console.WriteLine("[QuicSupport]   - Missing system libraries (libmsquic)");
            Console.WriteLine("[QuicSupport]   - Unsupported OS version");
            Console.WriteLine("[QuicSupport]   - .NET runtime configuration");
        }
        else
        {
            Console.WriteLine("[QuicSupport] ✓ QUIC is supported");
        }
    }
}

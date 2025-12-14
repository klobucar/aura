using System;
using System.Runtime.InteropServices;

namespace Aura.Desktop;

/// <summary>
/// Native library loader for libmsquic on macOS.
/// </summary>
public static class MsQuicLoader
{
    [DllImport("libdl.dylib")]
    private static extern IntPtr dlopen(string filename, int flags);
    
    [DllImport("libdl.dylib")]
    private static extern IntPtr dlerror();
    
    private const int RTLD_NOW = 2;
    private const int RTLD_GLOBAL = 8;
    
    public static bool TryLoadMsQuic()
    {
        if (!RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
        {
            Console.WriteLine("[MsQuicLoader] Not on macOS, skipping manual load");
            return true;
        }
        
        var paths = new[]
        {
            "/opt/homebrew/lib/libmsquic.dylib",
            "/opt/homebrew/lib/libmsquic.2.dylib",
            "/usr/local/lib/libmsquic.dylib",
            "libmsquic.dylib"
        };
        
        foreach (var path in paths)
        {
            Console.WriteLine($"[MsQuicLoader] Trying to load: {path}");
            var handle = dlopen(path, RTLD_NOW | RTLD_GLOBAL);
            
            if (handle != IntPtr.Zero)
            {
                Console.WriteLine($"[MsQuicLoader] ✓ Successfully loaded libmsquic from: {path}");
                return true;
            }
            
            var error = dlerror();
            if (error != IntPtr.Zero)
            {
                var errorMsg = Marshal.PtrToStringAnsi(error);
                Console.WriteLine($"[MsQuicLoader] Failed to load {path}: {errorMsg}");
            }
        }
        
        Console.WriteLine("[MsQuicLoader] ❌ Failed to load libmsquic from any path");
        return false;
    }
}

using System;
using System.IO;
using System.Runtime.InteropServices;

namespace Aura.Desktop.Services;

/// <summary>
/// Configures native library loading for UniFFI-generated bindings.
/// Ensures libaura_core.dylib can be found on macOS.
/// </summary>
public static class NativeLibraryLoader
{
    private static bool _initialized = false;
    
    public static void Initialize()
    {
        if (_initialized) return;
        _initialized = true;
        
        Console.WriteLine("[NativeLibraryLoader] Initializing custom DllImport resolver...");
        
        // Register custom resolver for aura_core library
        NativeLibrary.SetDllImportResolver(typeof(NativeLibraryLoader).Assembly, ResolveDllImport);
        
        Console.WriteLine("[NativeLibraryLoader] DllImport resolver registered");
    }
    
    private static IntPtr ResolveDllImport(string libraryName, System.Reflection.Assembly assembly, DllImportSearchPath? searchPath)
    {
        // Only handle aura_core library
        if (libraryName != "aura_core")
            return IntPtr.Zero; // Let default resolution handle it
        
        Console.WriteLine($"[NativeLibraryLoader] Resolving: {libraryName}");
        
        // Get the directory where the executable is located
        string? exeDir = Path.GetDirectoryName(Environment.ProcessPath);
        if (exeDir == null)
        {
            Console.WriteLine("[NativeLibraryLoader] Could not determine executable directory");
            return IntPtr.Zero;
        }
        
        // Try different library names based on platform
        string[] candidateNames = RuntimeInformation.IsOSPlatform(OSPlatform.Windows)
            ? new[] { "aura_core.dll", "libaura_core.dll" }
            : RuntimeInformation.IsOSPlatform(OSPlatform.OSX)
                ? new[] { "libaura_core.dylib", "aura_core.dylib" }
                : new[] { "libaura_core.so", "aura_core.so" };
        
        foreach (var name in candidateNames)
        {
            string fullPath = Path.Combine(exeDir, name);
            Console.WriteLine($"[NativeLibraryLoader] Trying: {fullPath}");
            
            if (File.Exists(fullPath))
            {
                Console.WriteLine($"[NativeLibraryLoader] Found library: {fullPath}");
                
                if (NativeLibrary.TryLoad(fullPath, out IntPtr handle))
                {
                    Console.WriteLine($"[NativeLibraryLoader] ✓ Successfully loaded: {fullPath}");
                    return handle;
                }
                else
                {
                    Console.WriteLine($"[NativeLibraryLoader] ✗ Failed to load: {fullPath}");
                }
            }
        }
        
        Console.WriteLine($"[NativeLibraryLoader] Could not find {libraryName}");
        return IntPtr.Zero;
    }
}

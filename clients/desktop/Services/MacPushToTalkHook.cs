using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Runtime.Versioning;
using Avalonia.Input;

namespace Aura.Desktop.Services;

/// <summary>
/// System-wide push-to-talk on macOS: a listen-only Quartz event tap, so the
/// key works while another app has focus.
///
/// Listen-only taps need the Input Monitoring permission (not Accessibility,
/// which only taps that can modify or drop events need). The tap's run loop
/// source goes on the main run loop, which Avalonia's UI thread drives, so the
/// callback runs on the UI thread, like the Windows hook's.
/// </summary>
[SupportedOSPlatform("macos")]
public sealed class MacPushToTalkHook : IGlobalPushToTalkHook
{
    private const string CoreGraphics = "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics";
    private const string CoreFoundation = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";

    private const uint kCGSessionEventTap = 1;
    private const uint kCGHeadInsertEventTap = 0;
    private const uint kCGEventTapOptionListenOnly = 1;

    private const uint kCGEventKeyDown = 10;
    private const uint kCGEventKeyUp = 11;
    private const uint kCGEventTapDisabledByTimeout = 0xFFFFFFFE;
    private const uint kCGEventTapDisabledByUserInput = 0xFFFFFFFF;

    private const int kCGKeyboardEventKeycode = 9;

    private const ulong kCGEventFlagMaskShift = 0x00020000;
    private const ulong kCGEventFlagMaskControl = 0x00040000;
    private const ulong kCGEventFlagMaskAlternate = 0x00080000;
    private const ulong kCGEventFlagMaskCommand = 0x00100000;

    /// <summary>System Settings › Privacy &amp; Security › Input Monitoring.</summary>
    private const string InputMonitoringSettingsUrl =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate IntPtr CGEventTapCallBack(IntPtr proxy, uint type, IntPtr cgEvent, IntPtr userInfo);

    [DllImport(CoreGraphics)]
    private static extern IntPtr CGEventTapCreate(uint tap, uint place, uint options, ulong eventsOfInterest,
        CGEventTapCallBack callback, IntPtr userInfo);

    [DllImport(CoreGraphics)]
    private static extern void CGEventTapEnable(IntPtr tap, [MarshalAs(UnmanagedType.I1)] bool enable);

    [DllImport(CoreGraphics)]
    private static extern long CGEventGetIntegerValueField(IntPtr cgEvent, int field);

    [DllImport(CoreGraphics)]
    private static extern ulong CGEventGetFlags(IntPtr cgEvent);

    [DllImport(CoreGraphics)]
    [return: MarshalAs(UnmanagedType.I1)]
    private static extern bool CGPreflightListenEventAccess();

    [DllImport(CoreGraphics)]
    [return: MarshalAs(UnmanagedType.I1)]
    private static extern bool CGRequestListenEventAccess();

    [DllImport(CoreFoundation)]
    private static extern IntPtr CFMachPortCreateRunLoopSource(IntPtr allocator, IntPtr port, nint order);

    [DllImport(CoreFoundation)]
    private static extern void CFMachPortInvalidate(IntPtr port);

    [DllImport(CoreFoundation)]
    private static extern IntPtr CFRunLoopGetMain();

    [DllImport(CoreFoundation)]
    private static extern void CFRunLoopAddSource(IntPtr runLoop, IntPtr source, IntPtr mode);

    [DllImport(CoreFoundation)]
    private static extern void CFRunLoopRemoveSource(IntPtr runLoop, IntPtr source, IntPtr mode);

    [DllImport(CoreFoundation)]
    private static extern void CFRelease(IntPtr cf);

    /// <summary><c>kCFRunLoopCommonModes</c> is an exported CFStringRef variable, not a function.</summary>
    private static readonly Lazy<IntPtr> CommonModesLazy = new(() =>
        Marshal.ReadIntPtr(NativeLibrary.GetExport(NativeLibrary.Load(CoreFoundation), "kCFRunLoopCommonModes")));

    private static IntPtr CommonModes() => CommonModesLazy.Value;

    // Held in a field: if the delegate were collected while the tap is live,
    // the next keystroke would call into freed memory.
    private readonly CGEventTapCallBack _callback;
    private IntPtr _tap;
    private IntPtr _source;
    private bool _held;

    public event Action? Pressed;
    public event Action? Released;

    public PushToTalkBinding Binding { get; set; } = PushToTalkBinding.Default;
    public bool Enabled { get; set; }
    public bool IsInstalled => _tap != IntPtr.Zero;

    public MacPushToTalkHook()
    {
        _callback = Callback;
    }

    /// <summary>Whether Input Monitoring is granted. Cheap; safe to poll.</summary>
    public static bool HasPermission => CGPreflightListenEventAccess();

    /// <summary>
    /// Ask for Input Monitoring. macOS shows its prompt only the first time;
    /// after that this just returns the current state.
    /// </summary>
    public static bool RequestPermission() => CGRequestListenEventAccess();

    public static void OpenPermissionSettings()
    {
        try
        {
            var psi = new ProcessStartInfo("/usr/bin/open") { UseShellExecute = false };
            psi.ArgumentList.Add(InputMonitoringSettingsUrl);
            using var process = Process.Start(psi);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[PTT] Could not open System Settings: {ex.Message}");
        }
    }

    public bool Install()
    {
        if (_tap != IntPtr.Zero) return true;
        if (!HasPermission) return false;

        const ulong mask = (1UL << (int)kCGEventKeyDown) | (1UL << (int)kCGEventKeyUp);
        var tap = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionListenOnly,
            mask, _callback, IntPtr.Zero);
        if (tap == IntPtr.Zero) return false;

        var source = CFMachPortCreateRunLoopSource(IntPtr.Zero, tap, 0);
        if (source == IntPtr.Zero)
        {
            CFMachPortInvalidate(tap);
            CFRelease(tap);
            return false;
        }

        CFRunLoopAddSource(CFRunLoopGetMain(), source, CommonModes());
        CGEventTapEnable(tap, true);
        _tap = tap;
        _source = source;
        return true;
    }

    private IntPtr Callback(IntPtr proxy, uint type, IntPtr cgEvent, IntPtr userInfo)
    {
        // An exception must not unwind into CoreGraphics.
        try
        {
            if (type is kCGEventTapDisabledByTimeout or kCGEventTapDisabledByUserInput)
            {
                // macOS switches off a tap it thinks is slow; without this, PTT
                // would silently stop working until the next launch.
                if (_tap != IntPtr.Zero) CGEventTapEnable(_tap, true);
            }
            else if (Enabled && Binding.MacKeyCode is { } target &&
                     CGEventGetIntegerValueField(cgEvent, kCGKeyboardEventKeycode) == target)
            {
                if (type == kCGEventKeyDown && !_held && ModifiersHeld(CGEventGetFlags(cgEvent)))
                {
                    _held = true;
                    Pressed?.Invoke();
                }
                else if (type == kCGEventKeyUp && _held)
                {
                    _held = false;
                    Released?.Invoke();
                }
            }
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[PTT] Event tap callback failed: {ex.Message}");
        }

        // Listen-only: the return value is ignored, the event goes on unchanged.
        return cgEvent;
    }

    private bool ModifiersHeld(ulong flags)
    {
        var mods = Binding.Modifiers;
        if (mods.HasFlag(KeyModifiers.Control) && (flags & kCGEventFlagMaskControl) == 0) return false;
        if (mods.HasFlag(KeyModifiers.Alt) && (flags & kCGEventFlagMaskAlternate) == 0) return false;
        if (mods.HasFlag(KeyModifiers.Shift) && (flags & kCGEventFlagMaskShift) == 0) return false;
        if (mods.HasFlag(KeyModifiers.Meta) && (flags & kCGEventFlagMaskCommand) == 0) return false;
        return true;
    }

    public void Dispose()
    {
        if (_tap != IntPtr.Zero)
        {
            CGEventTapEnable(_tap, false);
            CFRunLoopRemoveSource(CFRunLoopGetMain(), _source, CommonModes());
            CFMachPortInvalidate(_tap);
            CFRelease(_source);
            CFRelease(_tap);
            _tap = IntPtr.Zero;
            _source = IntPtr.Zero;
        }
        if (_held)
        {
            _held = false;
            Released?.Invoke();
        }
    }
}

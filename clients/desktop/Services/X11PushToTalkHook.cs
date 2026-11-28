using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Runtime.Versioning;
using System.Threading;
using Avalonia.Input;

namespace Aura.Desktop.Services;

/// <summary>
/// System-wide push-to-talk on an X11 session: XInput2 raw key events on the
/// root window, which arrive whichever window has focus and need no special
/// permission. They are observed only; every key still reaches the focused
/// application.
///
/// The hook opens its own X connection and reads it on its own thread, so it
/// never touches Avalonia's connection. <see cref="Pressed"/> and
/// <see cref="Released"/> fire on that thread.
///
/// Not for Wayland: under XWayland, raw events only arrive while an X11
/// window has focus. <see cref="PortalPushToTalkHook"/> covers Wayland.
/// </summary>
[SupportedOSPlatform("linux")]
public sealed class X11PushToTalkHook : IGlobalPushToTalkHook
{
    private const string LibX11 = "libX11.so.6";
    private const string LibXi = "libXi.so.6";

    private const int GenericEvent = 35;
    private const int XI_RawKeyPress = 13;
    private const int XI_RawKeyRelease = 14;
    private const int XIAllMasterDevices = 1;

    // XEvent is a union padded to 24 longs.
    private const int XEventSize = 24 * 8;

    // XGenericEventCookie / XIRawEvent field offsets (LP64).
    private const int CookieExtension = 32;
    private const int CookieEvtype = 36;
    private const int CookieData = 48;
    private const int RawEventDetail = 56;

    [StructLayout(LayoutKind.Sequential)]
    private struct XIEventMask
    {
        public int DeviceId;
        public int MaskLen;
        public IntPtr Mask;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PollFd
    {
        public int Fd;
        public short Events;
        public short Revents;
    }

    [DllImport(LibX11)] private static extern IntPtr XOpenDisplay(IntPtr name);
    [DllImport(LibX11)] private static extern int XCloseDisplay(IntPtr display);
    [DllImport(LibX11)] private static extern IntPtr XDefaultRootWindow(IntPtr display);
    [DllImport(LibX11)] private static extern int XConnectionNumber(IntPtr display);
    [DllImport(LibX11)] private static extern int XFlush(IntPtr display);
    [DllImport(LibX11)] private static extern int XPending(IntPtr display);
    [DllImport(LibX11)] private static extern int XNextEvent(IntPtr display, IntPtr xevent);
    [DllImport(LibX11)] private static extern int XGetEventData(IntPtr display, IntPtr cookie);
    [DllImport(LibX11)] private static extern void XFreeEventData(IntPtr display, IntPtr cookie);
    [DllImport(LibX11)] private static extern byte XKeysymToKeycode(IntPtr display, IntPtr keysym);
    [DllImport(LibX11)] private static extern int XQueryKeymap(IntPtr display, byte[] keys);

    [DllImport(LibX11)]
    private static extern int XQueryExtension(IntPtr display, string name,
        out int majorOpcode, out int firstEvent, out int firstError);

    [DllImport(LibXi)] private static extern int XIQueryVersion(IntPtr display, ref int major, ref int minor);
    [DllImport(LibXi)] private static extern int XISelectEvents(IntPtr display, IntPtr window, ref XIEventMask mask, int numMasks);

    [DllImport("libc", SetLastError = true)]
    private static extern int poll(ref PollFd fds, ulong nfds, int timeout);

    private const short POLLIN = 0x1;

    private IntPtr _display;
    private int _opcode;
    private Thread? _thread;
    private volatile bool _stop;
    private volatile PushToTalkBinding _binding = PushToTalkBinding.Default;
    private volatile bool _enabled;

    // Resolved on the hook thread from the binding, against the user's layout.
    private PushToTalkBinding? _resolvedFor;
    private int _keycode;
    private readonly Dictionary<KeyModifiers, byte[]> _modifierKeycodes = new();
    private bool _held;

    public event Action? Pressed;
    public event Action? Released;
    // Installs synchronously; IsInstalled never changes later.
    public event Action? StateChanged { add { } remove { } }

    public PushToTalkBinding Binding
    {
        get => _binding;
        set => _binding = value;
    }

    public bool Enabled
    {
        get => _enabled;
        set => _enabled = value;
    }

    public bool IsInstalled => _thread != null;

    /// <summary>An X11 session: DISPLAY set and no Wayland compositor.</summary>
    public static bool IsAvailable =>
        !string.IsNullOrEmpty(Environment.GetEnvironmentVariable("DISPLAY")) &&
        !PortalPushToTalkHook.IsWaylandSession;

    public bool Install()
    {
        if (_thread != null) return true;
        if (!IsAvailable) return false;

        try
        {
            _display = XOpenDisplay(IntPtr.Zero);
            if (_display == IntPtr.Zero) return false;

            if (XQueryExtension(_display, "XInputExtension", out _opcode, out _, out _) == 0)
            {
                Close();
                return false;
            }

            int major = 2, minor = 0;
            if (XIQueryVersion(_display, ref major, ref minor) != 0)
            {
                Close();
                return false;
            }

            var maskBytes = new byte[4];
            maskBytes[XI_RawKeyPress >> 3] |= (byte)(1 << (XI_RawKeyPress & 7));
            maskBytes[XI_RawKeyRelease >> 3] |= (byte)(1 << (XI_RawKeyRelease & 7));
            var maskPtr = Marshal.AllocHGlobal(maskBytes.Length);
            try
            {
                Marshal.Copy(maskBytes, 0, maskPtr, maskBytes.Length);
                var mask = new XIEventMask { DeviceId = XIAllMasterDevices, MaskLen = maskBytes.Length, Mask = maskPtr };
                XISelectEvents(_display, XDefaultRootWindow(_display), ref mask, 1);
                XFlush(_display);
            }
            finally
            {
                Marshal.FreeHGlobal(maskPtr);
            }
        }
        catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException)
        {
            // libXi missing: stay in-window.
            Console.WriteLine($"[PTT] X11 hook unavailable: {ex.Message}");
            Close();
            return false;
        }

        _stop = false;
        _thread = new Thread(Run) { IsBackground = true, Name = "Aura PTT (X11)" };
        _thread.Start();
        return true;
    }

    private void Run()
    {
        var xevent = Marshal.AllocHGlobal(XEventSize);
        var pollFd = new PollFd { Fd = XConnectionNumber(_display), Events = POLLIN };
        try
        {
            while (!_stop)
            {
                while (!_stop && XPending(_display) > 0)
                {
                    XNextEvent(_display, xevent);
                    try
                    {
                        Handle(xevent);
                    }
                    catch (Exception ex)
                    {
                        Console.WriteLine($"[PTT] X11 event failed: {ex.Message}");
                    }
                }
                // Wake at least every 200ms so Dispose isn't kept waiting.
                pollFd.Revents = 0;
                poll(ref pollFd, 1, 200);
            }
        }
        finally
        {
            Marshal.FreeHGlobal(xevent);
            Close();
        }
    }

    private void Handle(IntPtr xevent)
    {
        if (Marshal.ReadInt32(xevent) != GenericEvent) return;
        if (Marshal.ReadInt32(xevent, CookieExtension) != _opcode) return;
        if (XGetEventData(_display, xevent) == 0) return;
        try
        {
            var evtype = Marshal.ReadInt32(xevent, CookieEvtype);
            if (evtype != XI_RawKeyPress && evtype != XI_RawKeyRelease) return;
            if (!_enabled) return;

            Resolve();
            if (_keycode == 0) return;

            var data = Marshal.ReadIntPtr(xevent, CookieData);
            var keycode = Marshal.ReadInt32(data, RawEventDetail);
            if (keycode != _keycode) return;

            if (evtype == XI_RawKeyPress && !_held && ModifiersHeld())
            {
                _held = true;
                Pressed?.Invoke();
            }
            else if (evtype == XI_RawKeyRelease && _held)
            {
                _held = false;
                Released?.Invoke();
            }
        }
        finally
        {
            XFreeEventData(_display, xevent);
        }
    }

    /// <summary>Map the binding to keycodes once per binding change, on the hook thread.</summary>
    private void Resolve()
    {
        var binding = _binding;
        if (ReferenceEquals(binding, _resolvedFor)) return;
        _resolvedFor = binding;
        _held = false;

        _keycode = binding.X11Keysym is { } keysym ? XKeysymToKeycode(_display, (IntPtr)keysym) : 0;

        _modifierKeycodes.Clear();
        void Add(KeyModifiers modifier, params int[] keysyms)
        {
            var codes = new List<byte>();
            foreach (var sym in keysyms)
            {
                var code = XKeysymToKeycode(_display, (IntPtr)sym);
                if (code != 0) codes.Add(code);
            }
            _modifierKeycodes[modifier] = codes.ToArray();
        }
        Add(KeyModifiers.Control, 0xffe3, 0xffe4);           // Control_L, Control_R
        Add(KeyModifiers.Shift, 0xffe1, 0xffe2);             // Shift_L, Shift_R
        Add(KeyModifiers.Alt, 0xffe9, 0xffea);               // Alt_L, Alt_R
        Add(KeyModifiers.Meta, 0xffeb, 0xffec);              // Super_L, Super_R
    }

    /// <summary>
    /// Raw events carry no modifier state, so ask the server which keys are
    /// down. That also covers a modifier pressed before the hook started.
    /// </summary>
    private bool ModifiersHeld()
    {
        var mods = _binding.Modifiers;
        if (mods == KeyModifiers.None) return true;

        var keys = new byte[32];
        XQueryKeymap(_display, keys);
        bool Down(byte code) => (keys[code >> 3] & (1 << (code & 7))) != 0;

        foreach (var modifier in new[] { KeyModifiers.Control, KeyModifiers.Shift, KeyModifiers.Alt, KeyModifiers.Meta })
        {
            if (!mods.HasFlag(modifier)) continue;
            if (!_modifierKeycodes.TryGetValue(modifier, out var codes)) return false;
            var any = false;
            foreach (var code in codes) any |= Down(code);
            if (!any) return false;
        }
        return true;
    }

    private void Close()
    {
        if (_display != IntPtr.Zero)
        {
            XCloseDisplay(_display);
            _display = IntPtr.Zero;
        }
    }

    public void Dispose()
    {
        _stop = true;
        var thread = _thread;
        _thread = null;
        if (thread != null && thread != Thread.CurrentThread) thread.Join(TimeSpan.FromSeconds(1));
        if (_held)
        {
            _held = false;
            Released?.Invoke();
        }
    }
}

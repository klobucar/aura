using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Runtime.Versioning;
using Avalonia.Input;

namespace Aura.Desktop.Services;

/// <summary>
/// How the microphone decides when to send. Persisted under
/// <c>AuraTransmissionMode</c> with the macOS raw values, so the stored setting
/// means the same thing on both clients.
/// </summary>
public enum TransmissionMode
{
    PushToTalk,
    VoiceActivation,
    AlwaysOn,
}

public static class TransmissionModes
{
    /// <summary>Display names, byte-for-byte the macOS <c>TransmissionMode.displayName</c>.</summary>
    public static string DisplayName(TransmissionMode mode) => mode switch
    {
        TransmissionMode.PushToTalk => "Push-to-Talk",
        TransmissionMode.VoiceActivation => "Voice Activation",
        _ => "Always On",
    };

    public static string ToSetting(TransmissionMode mode) => mode switch
    {
        TransmissionMode.PushToTalk => "ptt",
        TransmissionMode.VoiceActivation => "vad",
        _ => "always_on",
    };

    public static TransmissionMode Parse(string? value) => value switch
    {
        "ptt" => TransmissionMode.PushToTalk,
        "vad" => TransmissionMode.VoiceActivation,
        "always_on" => TransmissionMode.AlwaysOn,
        _ => TransmissionMode.PushToTalk,
    };

    /// <summary>
    /// VAD sensitivity (0..1) to a dBFS threshold, the macOS mapping exactly:
    /// 0 → −50 dB (picks up everything), 1 → −20 dB (loud speech only).
    /// </summary>
    public static float VadThresholdDb(double sensitivity) =>
        (float)(-50.0 + Math.Clamp(sensitivity, 0, 1) * 30.0);
}

/// <summary>
/// A push-to-talk key: one non-modifier key plus the modifiers that must be held
/// with it. Holding extra modifiers still counts — in a game you may well be
/// holding Shift to run when you start talking.
/// </summary>
public sealed record PushToTalkBinding(Key Key, KeyModifiers Modifiers)
{
    /// <summary>The handoff's desktop default: <c>Ctrl+Space</c>.</summary>
    public static PushToTalkBinding Default { get; } = new(Key.Space, KeyModifiers.Control);

    private static readonly HashSet<Key> ModifierKeys = new()
    {
        Key.LeftCtrl, Key.RightCtrl, Key.LeftAlt, Key.RightAlt, Key.LeftShift, Key.RightShift,
        Key.LWin, Key.RWin,
    };

    /// <summary>Modifier keys on their own can't be a binding: there would be no key to release.</summary>
    public static bool IsModifierKey(Key key) => ModifierKeys.Contains(key);

    /// <summary>`Ctrl+Space` — keycap text for the HUD chip and settings.</summary>
    public string Display
    {
        get
        {
            var parts = new List<string>();
            if (Modifiers.HasFlag(KeyModifiers.Control)) parts.Add("Ctrl");
            if (Modifiers.HasFlag(KeyModifiers.Alt)) parts.Add("Alt");
            if (Modifiers.HasFlag(KeyModifiers.Shift)) parts.Add("Shift");
            if (Modifiers.HasFlag(KeyModifiers.Meta)) parts.Add("Win");
            parts.Add(KeyName(Key));
            return string.Join("+", parts);
        }
    }

    public bool Matches(Key key, KeyModifiers modifiers) =>
        key == Key && (modifiers & Modifiers) == Modifiers;

    public string ToSetting() => $"{(int)Modifiers}:{Key}";

    public static PushToTalkBinding Parse(string? value)
    {
        if (string.IsNullOrWhiteSpace(value)) return Default;
        var parts = value.Split(':', 2);
        if (parts.Length == 2 &&
            int.TryParse(parts[0], out var mods) &&
            Enum.TryParse<Key>(parts[1], out var key) &&
            key != Key.None && !IsModifierKey(key))
        {
            return new PushToTalkBinding(key, (KeyModifiers)mods & (KeyModifiers.Control | KeyModifiers.Alt | KeyModifiers.Shift | KeyModifiers.Meta));
        }
        return Default;
    }

    private static string KeyName(Key key) => key switch
    {
        >= Key.D0 and <= Key.D9 => ((char)('0' + (key - Key.D0))).ToString(),
        >= Key.NumPad0 and <= Key.NumPad9 => "Num" + (key - Key.NumPad0),
        Key.OemTilde => "`",
        Key.OemMinus => "-",
        Key.OemPlus => "=",
        Key.OemOpenBrackets => "[",
        Key.OemCloseBrackets => "]",
        Key.OemSemicolon => ";",
        Key.OemQuotes => "'",
        Key.OemComma => ",",
        Key.OemPeriod => ".",
        Key.OemQuestion => "/",
        Key.OemPipe => "\\",
        Key.CapsLock => "Caps Lock",
        Key.Scroll => "Scroll Lock",
        Key.Next => "Page Down",
        Key.Prior => "Page Up",
        Key.Return => "Enter",
        _ => key.ToString(),
    };

    /// <summary>
    /// The Windows virtual-key code for <see cref="Key"/>, for the global hook.
    /// Null for keys the hook can't watch.
    /// </summary>
    public int? VirtualKey => Key switch
    {
        >= Key.A and <= Key.Z => 0x41 + (Key - Key.A),
        >= Key.D0 and <= Key.D9 => 0x30 + (Key - Key.D0),
        >= Key.F1 and <= Key.F24 => 0x70 + (Key - Key.F1),
        >= Key.NumPad0 and <= Key.NumPad9 => 0x60 + (Key - Key.NumPad0),
        Key.Space => 0x20,
        Key.Tab => 0x09,
        Key.Return => 0x0D,
        Key.CapsLock => 0x14,
        Key.Insert => 0x2D,
        Key.Delete => 0x2E,
        Key.Home => 0x24,
        Key.End => 0x23,
        Key.Prior => 0x21,
        Key.Next => 0x22,
        Key.Pause => 0x13,
        Key.Scroll => 0x91,
        Key.Up => 0x26,
        Key.Down => 0x28,
        Key.Left => 0x25,
        Key.Right => 0x27,
        Key.OemTilde => 0xC0,
        Key.OemMinus => 0xBD,
        Key.OemPlus => 0xBB,
        Key.OemOpenBrackets => 0xDB,
        Key.OemCloseBrackets => 0xDD,
        Key.OemSemicolon => 0xBA,
        Key.OemQuotes => 0xDE,
        Key.OemComma => 0xBC,
        Key.OemPeriod => 0xBE,
        Key.OemQuestion => 0xBF,
        Key.OemPipe => 0xDC,
        _ => null,
    };
}

/// <summary>
/// System-wide push-to-talk on Windows: a <c>WH_KEYBOARD_LL</c> hook, so the key
/// works while a game has focus — the whole point of PTT (handoff: "must become
/// a global hook").
///
/// The hook observes only; every key is passed on to the focused application.
/// It must be installed from a thread with a message loop — the Avalonia UI
/// thread is one — and its callback runs on that same thread, so it stays tiny.
/// </summary>
public sealed class GlobalPushToTalkHook : IDisposable
{
    private const int WH_KEYBOARD_LL = 13;
    private const int WM_KEYDOWN = 0x0100;
    private const int WM_KEYUP = 0x0101;
    private const int WM_SYSKEYDOWN = 0x0104;
    private const int WM_SYSKEYUP = 0x0105;

    private const int VK_SHIFT = 0x10;
    private const int VK_CONTROL = 0x11;
    private const int VK_MENU = 0x12;
    private const int VK_LWIN = 0x5B;
    private const int VK_RWIN = 0x5C;

    private delegate IntPtr LowLevelKeyboardProc(int nCode, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetWindowsHookExW(int idHook, LowLevelKeyboardProc lpfn, IntPtr hMod, uint dwThreadId);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnhookWindowsHookEx(IntPtr hhk);

    [DllImport("user32.dll")]
    private static extern IntPtr CallNextHookEx(IntPtr hhk, int nCode, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int vKey);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr GetModuleHandleW(string? lpModuleName);

    // Held in a field: if the delegate were collected while the hook is live,
    // the next keystroke anywhere on the system would call into freed memory.
    private readonly LowLevelKeyboardProc _proc;
    private IntPtr _hook;
    private bool _held;

    public event Action? Pressed;
    public event Action? Released;

    /// <summary>The binding to watch. Swapping it mid-hold releases cleanly.</summary>
    public PushToTalkBinding Binding { get; set; } = PushToTalkBinding.Default;

    /// <summary>When false, key events are ignored (but still passed through).</summary>
    public bool Enabled { get; set; }

    public bool IsInstalled => _hook != IntPtr.Zero;

    public GlobalPushToTalkHook()
    {
        _proc = Callback;
    }

    /// <summary>Install the hook. Returns false (and leaves PTT in-window) on failure.</summary>
    public bool Install()
    {
        if (_hook != IntPtr.Zero) return true;
        if (!OperatingSystem.IsWindows()) return false;
        _hook = SetWindowsHookExW(WH_KEYBOARD_LL, _proc, GetModuleHandleW(null), 0);
        return _hook != IntPtr.Zero;
    }

    private IntPtr Callback(int nCode, IntPtr wParam, IntPtr lParam)
    {
        if (nCode >= 0 && Enabled)
        {
            var vk = Marshal.ReadInt32(lParam);
            var message = (int)wParam;
            if (Binding.VirtualKey is { } target && vk == target)
            {
                if ((message == WM_KEYDOWN || message == WM_SYSKEYDOWN) && !_held && ModifiersHeld())
                {
                    _held = true;
                    Pressed?.Invoke();
                }
                else if ((message == WM_KEYUP || message == WM_SYSKEYUP) && _held)
                {
                    _held = false;
                    Released?.Invoke();
                }
            }
        }

        return CallNextHookEx(IntPtr.Zero, nCode, wParam, lParam);
    }

    private bool ModifiersHeld()
    {
        static bool Down(int vk) => (GetAsyncKeyState(vk) & 0x8000) != 0;
        var mods = Binding.Modifiers;
        if (mods.HasFlag(KeyModifiers.Control) && !Down(VK_CONTROL)) return false;
        if (mods.HasFlag(KeyModifiers.Alt) && !Down(VK_MENU)) return false;
        if (mods.HasFlag(KeyModifiers.Shift) && !Down(VK_SHIFT)) return false;
        if (mods.HasFlag(KeyModifiers.Meta) && !Down(VK_LWIN) && !Down(VK_RWIN)) return false;
        return true;
    }

    public void Dispose()
    {
        if (_hook != IntPtr.Zero)
        {
            UnhookWindowsHookEx(_hook);
            _hook = IntPtr.Zero;
        }
        if (_held)
        {
            _held = false;
            Released?.Invoke();
        }
    }
}

/// <summary>Reconnect backoff, the macOS policy: 1, 2, 4, 8, 16 s, five attempts.</summary>
public static class ReconnectPolicy
{
    public const int MaxAttempts = 5;

    public static TimeSpan DelayFor(int attempt) =>
        TimeSpan.FromSeconds(Math.Min(Math.Pow(2, Math.Max(0, attempt - 1)), 30));
}

/// <summary>
/// Who owns the mic during a push-to-talk hold. All state changes happen
/// synchronously in <see cref="Press"/> / <see cref="Release"/>, before any
/// await, so a key-up that arrives while the mic is still opening still
/// closes it — the previous code set "opened by PTT" only after the open
/// finished and a fast tap left the mic latched on.
/// </summary>
public sealed class PushToTalkLatch
{
    private bool _openedMic;

    public DateTime? HeldSince { get; private set; }
    public bool IsHeld => HeldSince != null;

    /// <summary>Key down. Returns true if the caller should open the mic.</summary>
    public bool Press(bool micOpen, DateTime now)
    {
        if (IsHeld) return false;
        HeldSince = now;
        _openedMic = !micOpen;
        return _openedMic;
    }

    /// <summary>Key up. Returns true if the caller should close the mic.</summary>
    public bool Release()
    {
        if (!IsHeld) return false;
        HeldSince = null;
        var close = _openedMic;
        _openedMic = false;
        return close;
    }

    /// <summary>A manual mic click takes the mic away from the hold: release leaves it alone.</summary>
    public void Disown() => _openedMic = false;
}

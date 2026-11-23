using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;

namespace Aura.Desktop.Services;

/// <summary>
/// Notification delivery while the window is in the background. The title
/// prefix (<c>(N) Aura</c>) is the view model's job; this adds what each
/// platform offers on top:
/// <list type="bullet">
/// <item>Windows: flash the taskbar button (FlashWindowEx).</item>
/// <item>Linux: a banner via <c>notify-send</c>, if it is on PATH.</item>
/// <item>macOS (this client's build): nothing beyond the title.</item>
/// </list>
/// </summary>
public static class DesktopNotifier
{
    [StructLayout(LayoutKind.Sequential)]
    private struct FLASHWINFO
    {
        public uint cbSize;
        public IntPtr hwnd;
        public uint dwFlags;
        public uint uCount;
        public uint dwTimeout;
    }

    private const uint FLASHW_TRAY = 0x2;
    private const uint FLASHW_TIMERNOFG = 0xC;

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool FlashWindowEx(ref FLASHWINFO pwfi);

    /// <summary>Flash the taskbar button until the window comes to the foreground.</summary>
    public static void FlashTaskbar(IntPtr hwnd)
    {
        if (!OperatingSystem.IsWindows() || hwnd == IntPtr.Zero) return;
        var info = new FLASHWINFO
        {
            cbSize = (uint)Marshal.SizeOf<FLASHWINFO>(),
            hwnd = hwnd,
            dwFlags = FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount = uint.MaxValue,
            dwTimeout = 0,
        };
        FlashWindowEx(ref info);
    }

    private static readonly Lazy<string?> NotifySendPath = new(() => FindOnPath("notify-send"));

    /// <summary>
    /// Show a Linux desktop banner. Arguments go through ArgumentList — never a
    /// shell — so message text can't be interpreted as anything but text.
    /// </summary>
    public static void ShowLinuxBanner(string title, string body)
    {
        if (!OperatingSystem.IsLinux() || NotifySendPath.Value is not { } exe) return;
        try
        {
            var psi = new ProcessStartInfo(exe)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            psi.ArgumentList.Add("--app-name=Aura");
            psi.ArgumentList.Add("--");
            psi.ArgumentList.Add(title);
            psi.ArgumentList.Add(body);
            using var process = Process.Start(psi);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[Notify] notify-send failed: {ex.Message}");
        }
    }

    /// <summary>Find an executable on PATH without invoking a shell.</summary>
    public static string? FindOnPath(string name)
    {
        var path = Environment.GetEnvironmentVariable("PATH");
        if (string.IsNullOrEmpty(path)) return null;
        foreach (var dir in path.Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries))
        {
            try
            {
                var candidate = Path.Combine(dir, name);
                if (File.Exists(candidate)) return candidate;
            }
            catch (ArgumentException)
            {
                // A malformed PATH entry is skipped.
            }
        }
        return null;
    }
}

/// <summary>
/// Per-user <c>aura://</c> registration on Windows, so clicking an invite link
/// launches this client with the link as its first argument. Writes to
/// HKCU\Software\Classes\aura only when the entry is missing or points at a
/// different executable. Nothing is registered on other platforms.
///
/// Forwarding a link to an already-running instance is out of scope: a second
/// click starts a second instance.
/// </summary>
public static class UrlSchemeRegistration
{
    public static void EnsureRegistered()
    {
        if (!OperatingSystem.IsWindows()) return;
        var exe = Environment.ProcessPath;
        if (string.IsNullOrEmpty(exe)) return;

        var command = $"\"{exe}\" \"%1\"";
        try
        {
            using var classes = Microsoft.Win32.Registry.CurrentUser.CreateSubKey(@"Software\Classes\aura");
            using var cmd = classes.CreateSubKey(@"shell\open\command");
            if (cmd.GetValue(null) as string == command && classes.GetValue("URL Protocol") != null) return;

            classes.SetValue(null, "URL:Aura invite");
            classes.SetValue("URL Protocol", "");
            cmd.SetValue(null, command);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[Invite] Couldn't register aura:// links: {ex.Message}");
        }
    }
}

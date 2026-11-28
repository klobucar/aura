using System;
using System.Collections.Generic;
using System.Runtime.Versioning;
using System.Threading.Tasks;
using Tmds.DBus.Protocol;

namespace Aura.Desktop.Services;

/// <summary>
/// System-wide push-to-talk on Wayland, through the desktop portal's
/// GlobalShortcuts interface (org.freedesktop.portal.GlobalShortcuts; KDE
/// Plasma, GNOME 48+ and others). Wayland gives applications no way to see
/// keys pressed in other windows, so the compositor owns the shortcut and
/// reports when it starts and stops being held.
///
/// The binding is only a preferred trigger: the desktop shows the user a
/// dialog, and they may confirm it or pick another key. Binding is
/// asynchronous, so <see cref="Install"/> starts it and returns true;
/// <see cref="IsInstalled"/> becomes true, and <see cref="StateChanged"/>
/// fires, once the shortcut is bound. Any failure (no portal, no
/// GlobalShortcuts, the user declined) leaves push-to-talk in-window.
/// <see cref="Pressed"/> and <see cref="Released"/> fire on a thread-pool thread.
/// </summary>
[SupportedOSPlatform("linux")]
public sealed class PortalPushToTalkHook : IGlobalPushToTalkHook
{
    private const string PortalBus = "org.freedesktop.portal.Desktop";
    private const string PortalPath = "/org/freedesktop/portal/desktop";
    private const string ShortcutsInterface = "org.freedesktop.portal.GlobalShortcuts";
    private const string RequestInterface = "org.freedesktop.portal.Request";
    private const string SessionInterface = "org.freedesktop.portal.Session";
    private const string ShortcutId = "push-to-talk";

    private DBusConnection? _connection;
    private readonly List<IDisposable> _watches = new();
    private string? _session;
    private volatile bool _installed;
    private volatile bool _enabled;
    private bool _started;
    private bool _held;
    private PushToTalkBinding _binding = PushToTalkBinding.Default;
    private readonly object _gate = new();

    public event Action? Pressed;
    public event Action? Released;
    public event Action? StateChanged;

    /// <summary>A Wayland session, where only the portal can see global keys.</summary>
    public static bool IsWaylandSession =>
        string.Equals(Environment.GetEnvironmentVariable("XDG_SESSION_TYPE"), "wayland", StringComparison.OrdinalIgnoreCase) ||
        !string.IsNullOrEmpty(Environment.GetEnvironmentVariable("WAYLAND_DISPLAY"));

    public PushToTalkBinding Binding
    {
        get => _binding;
        set
        {
            if (value == _binding) return;
            _binding = value;
            // The trigger is chosen when binding, so ask again with the new one.
            if (_started) _ = RestartAsync();
        }
    }

    public bool Enabled
    {
        get => _enabled;
        set => _enabled = value;
    }

    public bool IsInstalled => _installed;

    public bool Install()
    {
        if (_started) return true;
        if (!IsWaylandSession || DBusAddress.Session is null) return false;
        _started = true;
        _ = SetUpAsync();
        return true;
    }

    private async Task RestartAsync()
    {
        await TearDownAsync().ConfigureAwait(false);
        await SetUpAsync().ConfigureAwait(false);
    }

    private async Task SetUpAsync()
    {
        try
        {
            var connection = new DBusConnection(DBusAddress.Session!);
            await connection.ConnectAsync().ConfigureAwait(false);
            lock (_gate) _connection = connection;

            _watches.Add(await connection.WatchSignalAsync(null, PortalPath, ShortcutsInterface, "Activated",
                ReadShortcutSignal, (Notification<ShortcutSignal> n) => OnShortcut(n, pressed: true),
                ObserverFlags.None, false, null).ConfigureAwait(false));
            _watches.Add(await connection.WatchSignalAsync(null, PortalPath, ShortcutsInterface, "Deactivated",
                ReadShortcutSignal, (Notification<ShortcutSignal> n) => OnShortcut(n, pressed: false),
                ObserverFlags.None, false, null).ConfigureAwait(false));

            // CreateSession → Response carries the session handle.
            var created = await RequestAsync(connection, "CreateSession", "a{sv}", (writer, token) =>
                writer.WriteDictionary(new Dictionary<string, VariantValue>
                {
                    ["handle_token"] = token,
                    ["session_handle_token"] = NewToken(),
                })).ConfigureAwait(false);
            if (created.Code != 0 || !created.Results.TryGetValue("session_handle", out var handle))
            {
                Console.WriteLine($"[PTT] Portal session refused (response {created.Code})");
                return;
            }
            var session = handle.Type == VariantValueType.ObjectPath ? handle.GetObjectPathAsString() : handle.GetString();
            lock (_gate) _session = session;

            // BindShortcuts → the desktop asks the user to confirm the trigger.
            var trigger = _binding.PortalTrigger;
            var bound = await RequestAsync(connection, "BindShortcuts", "oa(sa{sv})sa{sv}", (writer, token) =>
            {
                writer.WriteObjectPath(session);
                var shortcuts = writer.WriteArrayStart(DBusType.Struct);
                writer.WriteStructureStart();
                writer.WriteString(ShortcutId);
                var properties = new Dictionary<string, VariantValue> { ["description"] = "Push to talk" };
                if (trigger != null) properties["preferred_trigger"] = trigger;
                writer.WriteDictionary(properties);
                writer.WriteArrayEnd(shortcuts);
                writer.WriteString(""); // no parent window handle
                writer.WriteDictionary(new Dictionary<string, VariantValue> { ["handle_token"] = token });
            }).ConfigureAwait(false);
            if (bound.Code != 0)
            {
                Console.WriteLine($"[PTT] Portal shortcut not bound (response {bound.Code})");
                return;
            }

            _installed = true;
            Console.WriteLine($"[PTT] Portal shortcut bound (preferred {trigger ?? "none"})");
            StateChanged?.Invoke();
        }
        catch (Exception ex)
        {
            // No portal, no GlobalShortcuts interface, or the bus went away.
            Console.WriteLine($"[PTT] Portal shortcuts unavailable: {ex.Message}");
        }
    }

    private readonly record struct ShortcutSignal(string Session, string Id);
    private readonly record struct Response(uint Code, Dictionary<string, VariantValue> Results);

    private static ShortcutSignal ReadShortcutSignal(Message message, object? state)
    {
        var reader = message.GetBodyReader();
        var session = reader.ReadObjectPathAsString();
        var id = reader.ReadString();
        return new ShortcutSignal(session, id);
    }

    private void OnShortcut(Notification<ShortcutSignal> notification, bool pressed)
    {
        if (notification.Type != NotificationType.Value) return;
        var signal = notification.Value;
        if (!_installed || signal.Id != ShortcutId || signal.Session != _session) return;
        if (pressed && !_held && _enabled)
        {
            _held = true;
            Pressed?.Invoke();
        }
        else if (!pressed && _held)
        {
            _held = false;
            Released?.Invoke();
        }
    }

    /// <summary>
    /// Call a portal method that answers through a Request object. The
    /// Response signal is watched before the call, at the path the portal
    /// derives from our bus name and token, so it can't be missed.
    /// </summary>
    private static async Task<Response> RequestAsync(DBusConnection connection, string method, string signature,
        Action<MessageWriter, string> writeBody)
    {
        var token = NewToken();
        var sender = connection.UniqueName!.TrimStart(':').Replace('.', '_');
        var requestPath = $"/org/freedesktop/portal/desktop/request/{sender}/{token}";

        var response = new TaskCompletionSource<Response>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var watch = await connection.WatchSignalAsync(null, requestPath, RequestInterface, "Response",
            static (Message message, object? state) =>
            {
                var reader = message.GetBodyReader();
                var code = reader.ReadUInt32();
                var results = reader.ReadDictionaryOfStringToVariantValue();
                return new Response(code, results);
            },
            (Notification<Response> n) =>
            {
                if (n.Type == NotificationType.Value) response.TrySetResult(n.Value);
                else response.TrySetException(n.Exception ?? new InvalidOperationException($"Portal request ended: {n.Type}"));
            }, ObserverFlags.None, false, null).ConfigureAwait(false);

        MessageBuffer call;
        using (var writer = connection.GetMessageWriter())
        {
            writer.WriteMethodCallHeader(PortalBus, PortalPath, ShortcutsInterface, method, signature, MessageFlags.None);
            writeBody(writer, token);
            call = writer.CreateMessage();
        }
        await connection.CallMethodAsync(call, static (Message message, object? state) =>
            message.GetBodyReader().ReadObjectPathAsString(), null).ConfigureAwait(false);

        // The user may take a while in the confirmation dialog.
        var finished = await Task.WhenAny(response.Task, Task.Delay(TimeSpan.FromMinutes(5))).ConfigureAwait(false);
        return finished == response.Task ? await response.Task.ConfigureAwait(false) : new Response(2, new());
    }

    private static string NewToken() => "aura" + Guid.NewGuid().ToString("N")[..12];

    private async Task TearDownAsync()
    {
        var wasInstalled = _installed;
        _installed = false;
        DBusConnection? connection;
        string? session;
        lock (_gate)
        {
            connection = _connection;
            session = _session;
            _connection = null;
            _session = null;
        }
        foreach (var watch in _watches) watch.Dispose();
        _watches.Clear();

        if (connection != null && session != null)
        {
            try
            {
                MessageBuffer close;
                using (var writer = connection.GetMessageWriter())
                {
                    writer.WriteMethodCallHeader(PortalBus, session, SessionInterface, "Close", null, MessageFlags.None);
                    close = writer.CreateMessage();
                }
                await connection.CallMethodAsync(close).ConfigureAwait(false);
            }
            catch (Exception)
            {
                // Closing the connection ends the session anyway.
            }
        }
        connection?.Dispose();

        if (_held)
        {
            _held = false;
            Released?.Invoke();
        }
        if (wasInstalled) StateChanged?.Invoke();
    }

    public void Dispose()
    {
        _started = false;
        _ = TearDownAsync();
    }
}

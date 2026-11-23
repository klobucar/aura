using System;
using System.Threading.Tasks;

namespace Aura.Desktop.Services;

/// <summary>
/// At most one run of an async operation at a time; callers that arrive while
/// it runs get the same task. Used for connecting: the reconnect timer, "Retry
/// now" and the Connect button used to start overlapping attempts that tore
/// down each other's clients and could leave a ghost session alive.
/// UI-thread only (no locking).
/// </summary>
public sealed class SingleFlight<T>
{
    private Task<T>? _current;

    public bool IsRunning => _current is { IsCompleted: false };

    public Task<T> Run(Func<Task<T>> operation)
    {
        if (_current is { IsCompleted: false } running) return running;
        _current = operation();
        return _current;
    }
}

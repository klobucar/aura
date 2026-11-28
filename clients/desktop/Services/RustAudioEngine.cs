using System;
using System.Threading;
using System.Threading.Tasks;
using uniffi.aura_core;

namespace Aura.Desktop.Services;

/// <summary>
/// C# wrapper for the Rust Audio Core (cpal).
/// Handles microphone capture and speaker playback via UniFFI AudioHardware.
/// </summary>
public class RustAudioEngine : IDisposable
{
    private readonly object _hardwareLock = new();
    private AudioHardware? _hardware;
    private string? _inputDevice;
    private string? _outputDevice;
    private bool _isCapturing;
    private CancellationTokenSource? _captureCts;

    /// <summary>Master output volume, 0.0–1.0 (attenuation only). Applied to mixed playback.</summary>
    public float Volume { get; set; } = 1.0f;

    /// <summary>
    /// Deafen. Playback is dropped rather than attenuated, so the master volume
    /// the user chose is untouched and comes back as it was on undeafen.
    /// </summary>
    public bool OutputMuted { get; set; }

    public event Action<byte[]>? OnAudioData;
    public event Action<string>? OnError;

    /// <summary>
    /// RMS level of each captured frame, 0..1 linear. Fires ~50×/s on the
    /// capture thread, so consumers must accumulate rather than marshal per
    /// frame. This is the only local level signal the client has: it drives the
    /// HUD input meter, the speaker card waveform, and — with the packet gate —
    /// the local user's talk lane.
    /// </summary>
    public event Action<float>? OnCaptureLevel;

    public string? FallbackNotice { get; private set; }

    public RustAudioEngine(string? inputDevice = null, string? outputDevice = null)
    {
        _inputDevice = NormalizeDeviceName(inputDevice);
        _outputDevice = NormalizeDeviceName(outputDevice);
        try
        {
            _hardware = CreateHardware(_inputDevice, _outputDevice);
        }
        catch (Exception ex) when (_inputDevice != null || _outputDevice != null)
        {
            FallbackNotice = $"Saved audio device unavailable ({ex.Message}); fell back to System Default.";
            _inputDevice = null;
            _outputDevice = null;
            _hardware = CreateHardware(null, null);
        }
    }

    private static string? NormalizeDeviceName(string? name) =>
        string.IsNullOrWhiteSpace(name) || string.Equals(name, "System Default", StringComparison.OrdinalIgnoreCase)
            ? null
            : name;

    private static AudioHardware CreateHardware(string? inputDevice, string? outputDevice)
    {
        var hw = (inputDevice != null || outputDevice != null)
            ? AudioHardware.NewWithDevices(inputDevice, outputDevice)
            : new AudioHardware();
        if (hw == null) throw new Exception("Failed to initialize Rust Audio Engine");
        // The output stream plays silence until it is started; capture is
        // started separately, when the mic opens.
        hw.StartPlayback();
        return hw;
    }

    /// <summary>
    /// Switch input/output audio devices (null or "System Default" uses OS default).
    /// Restarts capture if it was active.
    /// </summary>
    public void SetDevices(string? inputDevice, string? outputDevice)
    {
        var nextIn = NormalizeDeviceName(inputDevice);
        var nextOut = NormalizeDeviceName(outputDevice);
        if (string.Equals(_inputDevice, nextIn, StringComparison.Ordinal) &&
            string.Equals(_outputDevice, nextOut, StringComparison.Ordinal))
        {
            return;
        }

        var wasCapturing = _isCapturing;
        if (wasCapturing) StopCapture();

        lock (_hardwareLock)
        {
            _hardware?.Dispose();
            _hardware = null;
            _inputDevice = nextIn;
            _outputDevice = nextOut;
            try
            {
                _hardware = CreateHardware(_inputDevice, _outputDevice);
            }
            catch (Exception ex)
            {
                OnError?.Invoke($"Failed to switch audio devices: {ex.Message}");
                _inputDevice = null;
                _outputDevice = null;
                try
                {
                    _hardware = CreateHardware(null, null);
                }
                catch (Exception fallbackEx)
                {
                    OnError?.Invoke($"Default audio device unavailable: {fallbackEx.Message}");
                }
            }
        }

        if (wasCapturing) StartCapture();
    }

    public void StartCapture()
    {
        if (_isCapturing) return;

        lock (_hardwareLock)
        {
            if (_hardware == null) return;
            try
            {
                _hardware.StartCapture();
            }
            catch (Exception ex)
            {
                OnError?.Invoke($"Failed to start Rust audio capture: {ex.Message}");
                return;
            }
        }

        _isCapturing = true;
        _captureCts = new CancellationTokenSource();
        // On the thread pool, not the caller's: StartCapture is called from
        // the UI thread, and the loop's awaits would otherwise resume there,
        // putting every frame's read, encode, encrypt and send on it.
        var token = _captureCts.Token;
        _ = Task.Run(() => CaptureLoop(token));
    }

    public void StopCapture()
    {
        _captureCts?.Cancel();
        lock (_hardwareLock)
        {
            try { _hardware?.StopCapture(); } catch { }
        }
        _isCapturing = false;
    }

    private async Task CaptureLoop(CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            short[]? samples = null;
            lock (_hardwareLock)
            {
                if (_hardware != null)
                {
                    try { samples = _hardware.ReadCapture(); } catch { }
                }
            }

            if (samples is { Length: > 0 })
            {
                var data = new byte[samples.Length * 2];
                Buffer.BlockCopy(samples, 0, data, 0, data.Length);
                OnAudioData?.Invoke(data);
                OnCaptureLevel?.Invoke(FrameLevel(samples, samples.Length));
            }
            else
            {
                // Small sleep to avoid spinning if no data is ready
                try { await Task.Delay(5, ct).ConfigureAwait(false); } catch (OperationCanceledException) { break; }
            }
        }
    }

    /// <summary>RMS of one captured frame, normalised to 0..1.</summary>
    private static float FrameLevel(short[] samples, int count)
    {
        if (count <= 0) return 0f;

        double sum = 0;
        for (int i = 0; i < count; i++)
        {
            double sample = samples[i] / 32768.0;
            sum += sample * sample;
        }

        return (float)Math.Sqrt(sum / count);
    }

    public void PlayAudio(short[] pcmSamples)
    {
        if (OutputMuted || pcmSamples.Length == 0) return;

        var sampleCount = pcmSamples.Length;
        var vol = Volume;
        short[] sampleBuffer;
        if (vol < 0.999f)
        {
            sampleBuffer = new short[sampleCount];
            for (int i = 0; i < sampleCount; i++)
                sampleBuffer[i] = (short)(pcmSamples[i] * vol);
        }
        else
        {
            sampleBuffer = pcmSamples;
        }

        lock (_hardwareLock)
        {
            try { _hardware?.WritePlayback(sampleBuffer); } catch { }
        }
    }

    public void PlayAudio(byte[] pcmData)
    {
        if (OutputMuted || pcmData.Length < 2) return;

        // Convert byte[] back to short[]
        var sampleCount = pcmData.Length / 2;
        var sampleBuffer = new short[sampleCount];
        Buffer.BlockCopy(pcmData, 0, sampleBuffer, 0, sampleCount * 2);
        PlayAudio(sampleBuffer);
    }

    public void Dispose()
    {
        StopCapture();
        lock (_hardwareLock)
        {
            _hardware?.Dispose();
            _hardware = null;
        }
    }
}

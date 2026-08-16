using System;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;

namespace Aura.Desktop.Services;

/// <summary>
/// C# wrapper for the Rust Audio Core (cpal).
/// Handles microphone capture and speaker playback via P/Invoke.
/// </summary>
public class RustAudioEngine : IDisposable
{
    private const string LibName = "aura_core"; // UniFFI name (libaura_core.dylib or aura_core.dll)

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr aura_audio_new();

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    private static extern void aura_audio_free(IntPtr hw);

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    private static extern int aura_audio_start_capture(IntPtr hw);

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    private static extern int aura_audio_stop_capture(IntPtr hw);

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    private static extern int aura_audio_read_capture(IntPtr hw, short[] buf, nuint len);

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    private static extern int aura_audio_write_playback(IntPtr hw, short[] buf, nuint len);

    private IntPtr _handle;
    private bool _isCapturing;
    private CancellationTokenSource? _captureCts;

    /// <summary>Master output volume, 0.0–1.0 (attenuation only). Applied to mixed playback.</summary>
    public float Volume { get; set; } = 1.0f;

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

    public RustAudioEngine()
    {
        _handle = aura_audio_new();
        if (_handle == IntPtr.Zero)
            throw new Exception("Failed to initialize Rust Audio Engine");
    }

    public void StartCapture()
    {
        if (_isCapturing) return;

        if (aura_audio_start_capture(_handle) != 0)
        {
            OnError?.Invoke("Failed to start Rust audio capture");
            return;
        }

        _isCapturing = true;
        _captureCts = new CancellationTokenSource();
        _ = CaptureLoop(_captureCts.Token);
    }

    public void StopCapture()
    {
        _captureCts?.Cancel();
        aura_audio_stop_capture(_handle);
        _isCapturing = false;
    }

    private async Task CaptureLoop(CancellationToken ct)
    {
        var sampleBuffer = new short[960]; // 20ms frame
        var byteBuffer = new byte[1920];  // 16-bit = 2 bytes per sample

        while (!ct.IsCancellationRequested)
        {
            int read = aura_audio_read_capture(_handle, sampleBuffer, (nuint)sampleBuffer.Length);
            if (read > 0)
            {
                // Convert short[] to byte[] for protocol compatibility
                Buffer.BlockCopy(sampleBuffer, 0, byteBuffer, 0, read * 2);
                
                var data = new byte[read * 2];
                Array.Copy(byteBuffer, data, read * 2);
                OnAudioData?.Invoke(data);
                OnCaptureLevel?.Invoke(FrameLevel(sampleBuffer, read));
            }
            else
            {
                // Small sleep to avoid spinning if no data is ready
                await Task.Delay(5, ct);
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

    public void PlayAudio(byte[] pcmData)
    {
        if (_handle == IntPtr.Zero) return;

        // Convert byte[] back to short[]
        var sampleCount = pcmData.Length / 2;
        var sampleBuffer = new short[sampleCount];
        Buffer.BlockCopy(pcmData, 0, sampleBuffer, 0, pcmData.Length);

        // Apply master volume (skip the per-sample multiply at unity gain).
        var vol = Volume;
        if (vol < 0.999f)
        {
            for (int i = 0; i < sampleCount; i++)
                sampleBuffer[i] = (short)(sampleBuffer[i] * vol);
        }

        aura_audio_write_playback(_handle, sampleBuffer, (nuint)sampleCount);
    }

    public void Dispose()
    {
        StopCapture();
        if (_handle != IntPtr.Zero)
        {
            aura_audio_free(_handle);
            _handle = IntPtr.Zero;
        }
    }
}

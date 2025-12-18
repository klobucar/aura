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

    public event Action<byte[]>? OnAudioData;
    public event Action<string>? OnError;

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
            }
            else
            {
                // Small sleep to avoid spinning if no data is ready
                await Task.Delay(5, ct);
            }
        }
    }

    public void PlayAudio(byte[] pcmData)
    {
        if (_handle == IntPtr.Zero) return;

        // Convert byte[] back to short[]
        var sampleCount = pcmData.Length / 2;
        var sampleBuffer = new short[sampleCount];
        Buffer.BlockCopy(pcmData, 0, sampleBuffer, 0, pcmData.Length);

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

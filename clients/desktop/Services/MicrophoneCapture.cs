using System;
using System.Threading;
using System.Threading.Tasks;

namespace Aura.Desktop.Services;

/// <summary>
/// Cross-platform microphone capture using NAudio.
/// Captures 48kHz, 16-bit, mono PCM audio.
/// </summary>
public class MicrophoneCapture : IDisposable
{
    private NAudio.Wave.WaveInEvent? _waveIn;
    private bool _isRunning;
    private int _packetsSent;
    
    /// <summary>Audio format: 48kHz, 16-bit, mono.</summary>
    public const int SampleRate = 48000;
    public const int BitsPerSample = 16;
    public const int Channels = 1;
    public const int BufferMs = 20; // 20ms frames
    
    /// <summary>Samples per 20ms frame at 48kHz.</summary>
    public const int SamplesPerFrame = SampleRate * BufferMs / 1000; // 960 samples
    
    public bool IsRunning => _isRunning;
    public int PacketsSent => _packetsSent;
    
    public event Action<byte[]>? OnAudioData;
    public event Action<string>? OnError;
    
    /// <summary>
    /// Start capturing audio from default microphone.
    /// </summary>
    public void Start()
    {
        if (_isRunning) return;
        
        try
        {
            _waveIn = new NAudio.Wave.WaveInEvent
            {
                WaveFormat = new NAudio.Wave.WaveFormat(SampleRate, BitsPerSample, Channels),
                BufferMilliseconds = BufferMs
            };
            
            _waveIn.DataAvailable += (sender, e) =>
            {
                if (e.BytesRecorded > 0)
                {
                    var buffer = new byte[e.BytesRecorded];
                    Array.Copy(e.Buffer, buffer, e.BytesRecorded);
                    OnAudioData?.Invoke(buffer);
                    Interlocked.Increment(ref _packetsSent);
                }
            };
            
            _waveIn.RecordingStopped += (sender, e) =>
            {
                if (e.Exception != null)
                {
                    OnError?.Invoke($"Recording stopped: {e.Exception.Message}");
                }
            };
            
            _waveIn.StartRecording();
            _isRunning = true;
        }
        catch (Exception ex)
        {
            OnError?.Invoke($"Failed to start microphone: {ex.Message}");
        }
    }
    
    /// <summary>
    /// Stop capturing audio.
    /// </summary>
    public void Stop()
    {
        if (!_isRunning || _waveIn == null) return;
        
        try
        {
            _waveIn.StopRecording();
        }
        finally
        {
            _isRunning = false;
        }
    }
    
    public void Dispose()
    {
        Stop();
        _waveIn?.Dispose();
        _waveIn = null;
        GC.SuppressFinalize(this);
    }
}

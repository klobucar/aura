using System;
using NAudio.Wave;

namespace Aura.Desktop.Services;

public class AudioPlayback : IDisposable
{
    private readonly WaveOutEvent _waveOut;
    private readonly BufferedWaveProvider _waveProvider;
    
    public AudioPlayback()
    {
        // 48kHz Mono 16-bit (Matches Aura standard)
        var format = new WaveFormat(48000, 16, 1);
        _waveProvider = new BufferedWaveProvider(format)
        {
            DiscardOnBufferOverflow = true,
            BufferDuration = TimeSpan.FromSeconds(1) // 1s buffer
        };
        
        _waveOut = new WaveOutEvent();
        _waveOut.Init(_waveProvider);
        _waveOut.Play();
    }
    
    public void Enqueue(byte[] pcmData)
    {
        _waveProvider.AddSamples(pcmData, 0, pcmData.Length);
    }
    
    public void Dispose()
    {
        _waveOut.Stop();
        _waveOut.Dispose();
    }
}

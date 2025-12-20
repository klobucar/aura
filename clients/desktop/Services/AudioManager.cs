using System;
using System.Collections.Generic;
using System.Linq;
using uniffi.aura_core;

namespace Aura.Desktop.Services;

/// <summary>
/// Manages audio encoding/decoding using the Rust core via UniFFI bindings.
/// Wraps AudioSenderWrapper and AudioReceiverWrapper for Opus 1.6 + DRED + encryption.
/// </summary>
public class AudioManager : IDisposable
{
    private AudioSenderWrapper? _sender;
    private AudioReceiverWrapper? _receiver;
    
    /// <summary>
    /// Event fired when active speakers change (for UI indicators)
    /// </summary>
    public event Action<HashSet<uint>>? OnActiveSpeakersChanged;
    
    /// <summary>
    /// Last known active speakers
    /// </summary>
    private HashSet<uint> _lastActiveSpeakers = new();
    
    public bool IsInitialized => _sender != null && _receiver != null;
    
    /// <summary>
    /// Initialize audio sender and receiver with encryption key
    /// </summary>
    public void Initialize(uint sessionId, byte[] key)
    {
        Console.WriteLine($"[AudioManager] Initialize called: sessionId={sessionId}, keyLen={key.Length}");
        
        if (key.Length != 32)
            throw new ArgumentException("Key must be 32 bytes", nameof(key));
        
        Console.WriteLine("[AudioManager] Creating AudioSenderWrapper...");
        _sender = new AudioSenderWrapper(sessionId, key);
        Console.WriteLine("[AudioManager] Creating AudioReceiverWrapper...");
        _receiver = new AudioReceiverWrapper();
        Console.WriteLine("[AudioManager] Wrappers created");
        
        // Configure Opus 1.6 features
        _sender.SetDredDuration(10); // 100ms redundancy
        _receiver.SetJitterBufferMs(40); // 40ms jitter buffer
        Console.WriteLine("[AudioManager] Opus features configured");
        
        // Configure audio processing (Windows defaults)
        _sender.SetNoiseSuppressionEnabled(true);  // RNNoise ON
        _sender.SetWebrtcAecEnabled(true);         // AEC ON (for speakers)
        _sender.SetWebrtcNsEnabled(false);         // WebRTC NS OFF (use RNNoise)
        _sender.SetWebrtcAgcEnabled(true);         // AGC ON (normalize volume)
        Console.WriteLine("[AudioManager] Audio processing configured");
    }
    
    /// <summary>
    /// Process captured audio from microphone (int16 PCM → encrypted Opus packet)
    /// </summary>
    public byte[]? ProcessCapture(short[] pcm)
    {
        if (_sender == null) return null;
        
        // Convert int16 to float32 (-1.0 to 1.0) for Opus 1.6
        var floatSamples = new float[pcm.Length];
        for (int i = 0; i < pcm.Length; i++)
        {
            floatSamples[i] = pcm[i] / 32768.0f;
        }
        
        try
        {
            var packet = _sender.ProcessFloat(floatSamples);
            return packet;
        }
        catch (Exception)
        {
            return null;
        }
    }
    
    /// <summary>
    /// Add a remote sender for receiving audio
    /// </summary>
    public void AddRemoteSender(uint sessionId, byte[] key)
    {
        _receiver?.AddSender(sessionId, key, 0);
    }
    
    /// <summary>
    /// Remove a remote sender
    /// </summary>
    public void RemoveRemoteSender(uint sessionId)
    {
        _receiver?.RemoveSender(sessionId);
    }
    
    /// <summary>
    /// Process incoming encrypted audio packet
    /// </summary>
    public void OnPacket(byte[] data)
    {
        _receiver?.OnPacket(data);
    }
    
    /// <summary>
    /// Pop mixed audio for playback, returns PCM samples
    /// Also updates active speaker tracking
    /// </summary>
    public short[]? PopMixed()
    {
        var result = _receiver?.PopMixed();
        if (result == null) return null;
        
        // Check if speakers changed
        var newSpeakers = new HashSet<uint>(result.activeSpeakers);
        if (!newSpeakers.SetEquals(_lastActiveSpeakers))
        {
            _lastActiveSpeakers = newSpeakers;
            OnActiveSpeakersChanged?.Invoke(newSpeakers);
        }
        
        return result.pcm;
    }
    
    // Settings API
    
    public void SetNoiseSuppressionEnabled(bool enabled)
    {
        _sender?.SetNoiseSuppressionEnabled(enabled);
    }
    
    public void SetWebrtcAecEnabled(bool enabled)
    {
        _sender?.SetWebrtcAecEnabled(enabled);
    }
    
    public void SetWebrtcNsEnabled(bool enabled)
    {
        _sender?.SetWebrtcNsEnabled(enabled);
        // Auto-disable RNNoise when WebRTC NS is on
        if (enabled)
        {
            _sender?.SetNoiseSuppressionEnabled(false);
        }
    }
    
    public void SetWebrtcAgcEnabled(bool enabled)
    {
        _sender?.SetWebrtcAgcEnabled(enabled);
    }
    
    public void SetDredDuration(int frames)
    {
        _sender?.SetDredDuration(frames);
    }
    
    public void SetJitterBufferMs(uint ms)
    {
        _receiver?.SetJitterBufferMs(ms);
    }
    
    public void Dispose()
    {
        // UniFFI handles cleanup via Drop trait
        _sender = null;
        _receiver = null;
    }
}

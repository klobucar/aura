using System;
using System.Collections.Generic;
using System.Linq;
using uniffi.aura_core;

namespace Aura.Desktop.Services;

/// <summary>
/// Manages audio encoding/decoding using the Rust core via UniFFI bindings.
/// Wraps AudioSenderWrapper and AudioReceiverWrapper for Opus 1.6 + DRED + encryption.
///
/// Processing settings are remembered here rather than only forwarded, because
/// the sender is created later (after login) than the settings are applied
/// (at startup and on every toggle). Without that, <see cref="Initialize"/>
/// would silently reset the user's choices to the defaults.
/// </summary>
public class AudioManager : IDisposable
{
    private AudioSenderWrapper? _sender;
    private AudioReceiverWrapper? _receiver;

    /// <summary>
    /// True once the sender holds a key exported from the channel's MLS voice
    /// group. Until then nothing is sent: the bootstrap key is a public
    /// constant, so a frame encrypted under it would be readable by the server.
    /// </summary>
    private volatile bool _hasGroupKey;

    // Remembered settings, applied whenever the sender/receiver are (re)created.
    private bool _noiseSuppression = true;
    private bool _aec = true;
    private bool _webrtcNs;
    private bool _agc = true;
    private int _dredFrames = 10;
    private uint _jitterMs = 40;
    private int _opusBitrateKbps = 64;
    private bool _vadEnabled;
    private float _vadThresholdDb = -35f;
    private readonly Queue<short> _cueSamples = new();
    private readonly object _cueLock = new();

    /// <summary>
    /// Event fired when active speakers change (for UI indicators).
    ///
    /// This comes off the jitter buffer, i.e. from *decoded incoming* audio, so
    /// it can never contain the local session id. The local talking signal is
    /// <see cref="OnLocalCapture"/> — anything that needs "who is talking"
    /// including us has to merge the two.
    /// </summary>
    public event Action<HashSet<uint>>? OnActiveSpeakersChanged;

    /// <summary>
    /// Fired for every captured frame with whether the frame produced a packet.
    /// False means the send path dropped it — the VAD gate (voice-activity mode)
    /// or no group key yet — so callers should combine this with the capture
    /// level rather than trusting it alone. Fires on the capture thread.
    /// </summary>
    public event Action<bool>? OnLocalCapture;

    /// <summary>
    /// Last known active speakers
    /// </summary>
    private HashSet<uint> _lastActiveSpeakers = new();

    public bool IsInitialized => _sender != null && _receiver != null;

    /// <summary>Whether outgoing audio can be encrypted under a real group key.</summary>
    public bool HasGroupKey => _hasGroupKey;

    /// <summary>
    /// Create the sender and receiver. <paramref name="bootstrapKey"/> only
    /// satisfies the constructor; sending stays disabled until
    /// <see cref="UpdateSenderKey"/> installs an MLS-exported key.
    /// </summary>
    public void Initialize(uint sessionId, byte[] bootstrapKey)
    {
        if (bootstrapKey.Length != 32)
            throw new ArgumentException("Key must be 32 bytes", nameof(bootstrapKey));

        _hasGroupKey = false;
        _sender = new AudioSenderWrapper(sessionId, bootstrapKey);
        _receiver = new AudioReceiverWrapper();
        _lastActiveSpeakers = new HashSet<uint>();
        ApplyRememberedSettings();
    }

    private void ApplyRememberedSettings()
    {
        if (_sender != null)
        {
            _sender.SetDredDuration(_dredFrames);
            _sender.SetWebrtcAecEnabled(_aec);
            _sender.SetWebrtcAgcEnabled(_agc);
            _sender.SetWebrtcNsEnabled(_webrtcNs);
            // WebRTC NS and RNNoise are alternatives; NS wins when both are asked for.
            _sender.SetNoiseSuppressionEnabled(_noiseSuppression && !_webrtcNs);
            _sender.SetVadThresholdDb(_vadThresholdDb);
            _sender.SetVadEnabled(_vadEnabled);
            _sender.SetBitrate((uint)(_opusBitrateKbps * 1000));
        }
        _receiver?.SetJitterBufferMs(_jitterMs);
    }

    /// <summary>
    /// Install the sender key exported from the MLS voice group. This is what
    /// arms sending.
    /// </summary>
    public void UpdateSenderKey(byte[] key, ulong epoch)
    {
        if (_sender == null) return;
        _sender.UpdateKey(key, epoch);
        _hasGroupKey = true;
    }

    /// <summary>
    /// Stop sending until the next group key arrives — called on channel switch,
    /// so nothing goes out to the new room under the old room's key.
    /// </summary>
    public void ClearGroupKey() => _hasGroupKey = false;

    /// <summary>
    /// Update a remote sender's decryption key. Returns false if the receiver
    /// does not know that sender yet (the caller should add it).
    /// </summary>
    public bool UpdateRemoteSenderKey(uint sessionId, byte[] key, ushort epochHint)
    {
        if (_receiver == null) return false;
        try
        {
            return _receiver.UpdateSenderKey(sessionId, key, epochHint);
        }
        catch (Exception)
        {
            return false;
        }
    }

    /// <summary>
    /// Process captured audio from microphone (int16 PCM → encrypted Opus packet).
    /// Returns null when nothing should be sent: no group key yet, or the VAD
    /// gate judged the frame to be silence.
    /// </summary>
    public byte[]? ProcessCapture(short[] pcm)
    {
        if (_sender == null || !_hasGroupKey)
        {
            OnLocalCapture?.Invoke(false);
            return null;
        }

        // Convert int16 to float32 (-1.0 to 1.0) for Opus 1.6
        var floatSamples = new float[pcm.Length];
        for (int i = 0; i < pcm.Length; i++)
        {
            floatSamples[i] = pcm[i] / 32768.0f;
        }

        try
        {
            var packet = _sender.ProcessFloat(floatSamples);
            OnLocalCapture?.Invoke(packet != null);
            return packet;
        }
        catch (Exception)
        {
            OnLocalCapture?.Invoke(false);
            return null;
        }
    }

    /// <summary>
    /// Add a remote sender for receiving audio
    /// </summary>
    public void AddRemoteSender(uint sessionId, byte[] key, ushort epochHint = 0)
    {
        _receiver?.AddSender(sessionId, key, epochHint);
    }

    /// <summary>
    /// Remove a remote sender
    /// </summary>
    public void RemoveRemoteSender(uint sessionId)
    {
        _receiver?.RemoveSender(sessionId);
    }

    /// <summary>
    /// Set one sender's local playback gain (1.0 = unchanged). Local only — the
    /// server is never told, and nobody else's mix is affected.
    /// </summary>
    public void SetSenderGain(uint sessionId, float gain)
    {
        _receiver?.SetSenderGain(sessionId, gain);
    }

    /// <summary>
    /// Mute one sender locally. The sender keeps decoding so its Opus state
    /// stays consistent; the mixer just drops the samples.
    /// </summary>
    public void SetSenderMuted(uint sessionId, bool muted)
    {
        _receiver?.SetSenderMuted(sessionId, muted);
    }

    /// <summary>
    /// Process incoming encrypted audio packet
    /// </summary>
    public void OnPacket(byte[] data)
    {
        _receiver?.OnPacket(data);
    }

    /// <summary>
    /// Report an audio packet in the older format (never decrypted or played;
    /// the core logs once per sender that the peer must update).
    /// </summary>
    public void OnLegacyPacket(byte[] data)
    {
        _receiver?.NoteLegacyPacket(data);
    }

    /// <summary>Queue a short PCM cue to be mixed into the next 20ms playback frames.</summary>
    public void EnqueueSoundCue(short[] samples)
    {
        if (samples.Length == 0) return;
        lock (_cueLock)
        {
            foreach (var s in samples) _cueSamples.Enqueue(s);
        }
    }

    /// <summary>
    /// Pop mixed audio for playback, returns PCM samples
    /// Also updates active speaker tracking and mixes any queued sound cues.
    /// </summary>
    public short[]? PopMixed()
    {
        var result = _receiver?.PopMixed();
        if (result != null)
        {
            // Check if speakers changed
            var newSpeakers = new HashSet<uint>(result.ActiveSpeakers);
            if (!newSpeakers.SetEquals(_lastActiveSpeakers))
            {
                _lastActiveSpeakers = newSpeakers;
                OnActiveSpeakersChanged?.Invoke(newSpeakers);
            }
        }

        var pcm = result?.Pcm;
        lock (_cueLock)
        {
            if (_cueSamples.Count == 0) return pcm;

            const int frameSize = 960; // 20ms at 48kHz
            if (pcm == null || pcm.Length == 0)
            {
                var count = Math.Min(frameSize, _cueSamples.Count);
                var cueFrame = new short[count];
                for (int i = 0; i < count; i++) cueFrame[i] = _cueSamples.Dequeue();
                return cueFrame;
            }

            var mixed = new short[pcm.Length];
            for (int i = 0; i < pcm.Length; i++)
            {
                int sample = pcm[i];
                if (_cueSamples.Count > 0) sample += _cueSamples.Dequeue();
                mixed[i] = (short)Math.Clamp(sample, short.MinValue, short.MaxValue);
            }
            return mixed;
        }
    }

    // Settings API

    public void SetNoiseSuppressionEnabled(bool enabled)
    {
        _noiseSuppression = enabled;
        _sender?.SetNoiseSuppressionEnabled(enabled && !_webrtcNs);
    }

    public void SetWebrtcAecEnabled(bool enabled)
    {
        _aec = enabled;
        _sender?.SetWebrtcAecEnabled(enabled);
    }

    public void SetWebrtcNsEnabled(bool enabled)
    {
        _webrtcNs = enabled;
        _sender?.SetWebrtcNsEnabled(enabled);
        // RNNoise and WebRTC NS are alternatives; WebRTC NS wins when on.
        _sender?.SetNoiseSuppressionEnabled(_noiseSuppression && !enabled);
    }

    public void SetWebrtcAgcEnabled(bool enabled)
    {
        _agc = enabled;
        _sender?.SetWebrtcAgcEnabled(enabled);
    }

    public void SetDredDuration(int frames)
    {
        _dredFrames = frames;
        _sender?.SetDredDuration(frames);
    }

    public void SetJitterBufferMs(uint ms)
    {
        _jitterMs = ms;
        _receiver?.SetJitterBufferMs(ms);
    }

    /// <summary>Set Opus encoder bitrate in kbps (clamped to 12–128 kbps).</summary>
    public void SetOpusBitrate(int kbps)
    {
        _opusBitrateKbps = Math.Clamp(kbps, 12, 128);
        _sender?.SetBitrate((uint)(_opusBitrateKbps * 1000));
    }

    /// <summary>Poll receiver call quality stats from AudioReceiverWrapper.</summary>
    public CallQualityStats? GetReceiverStats()
    {
        if (_receiver == null) return null;
        var s = _receiver.GetStats();
        return new CallQualityStats(s.PacketsReceived, s.PacketsLost, s.PlcCount, s.EstimatedJitterMs);
    }

    /// <summary>
    /// Voice-activity gate. On, silent frames produce no packet at all (and the
    /// sequence number holds), which is what "voice activity" transmission means.
    /// </summary>
    public void SetVadEnabled(bool enabled)
    {
        _vadEnabled = enabled;
        _sender?.SetVadEnabled(enabled);
    }

    /// <summary>VAD threshold in dBFS.</summary>
    public void SetVadThresholdDb(float thresholdDb)
    {
        _vadThresholdDb = thresholdDb;
        _sender?.SetVadThresholdDb(thresholdDb);
    }

    public void Dispose()
    {
        // UniFFI handles cleanup via Drop trait
        _hasGroupKey = false;
        _sender = null;
        _receiver = null;
    }
}

/// <summary>Snapshot of receiver call quality metrics.</summary>
public sealed record CallQualityStats(
    ulong PacketsReceived,
    ulong PacketsLost,
    ulong PlcCount,
    double JitterMs)
{
    public double LossPercent
    {
        get
        {
            var total = PacketsReceived + PacketsLost;
            return total == 0 ? 0.0 : (PacketsLost * 100.0) / total;
        }
    }

    public string FormatSummary() =>
        $"Loss: {LossPercent:0.0}% ({PacketsLost} lost) · PLC: {PlcCount} · Jitter: {JitterMs:0.0} ms";
}

/// <summary>
/// Generates subtle, non-blocking synthesized PCM audio cues for voice events
/// (mute, unmute, join, leave), mixing into <see cref="AudioManager"/> frames
/// during a call or playing via <see cref="RustAudioEngine"/> on disconnect.
/// </summary>
public static class VoiceSoundCues
{
    public enum CueKind { Mute, Unmute, Join, Leave }

    public static void Play(AudioManager? audioManager, RustAudioEngine? engine, CueKind kind)
    {
        if (engine is { OutputMuted: true }) return;
        var samples = Synthesize(kind);
        if (audioManager is { IsInitialized: true })
        {
            audioManager.EnqueueSoundCue(samples);
            return;
        }
        if (engine != null)
        {
            _ = System.Threading.Tasks.Task.Run(() => engine.PlayAudio(samples));
        }
    }

    public static short[] Synthesize(CueKind kind)
    {
        const int sampleRate = 48000;
        var (f1, f2) = kind switch
        {
            CueKind.Mute => (520.0, 390.0),
            CueKind.Unmute => (390.0, 560.0),
            CueKind.Join => (440.0, 660.0),
            CueKind.Leave => (587.0, 392.0),
            _ => (440.0, 440.0),
        };

        const int halfSamples = sampleRate * 35 / 1000; // 35ms per tone = 70ms total
        var pcm = new short[halfSamples * 2];
        const double amp = 2200.0; // subtle, ~ -23 dBFS

        for (int i = 0; i < pcm.Length; i++)
        {
            bool second = i >= halfSamples;
            int localIdx = second ? i - halfSamples : i;
            double freq = second ? f2 : f1;
            double env = Math.Sin(Math.PI * localIdx / halfSamples);
            double t = (double)localIdx / sampleRate;
            pcm[i] = (short)(Math.Sin(2.0 * Math.PI * freq * t) * env * amp);
        }
        return pcm;
    }
}

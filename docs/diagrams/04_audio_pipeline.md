# Audio Pipeline Data Flow

This diagram details the transformation of audio data from microphone to network packet.

```mermaid
flowchart LR
    Mic((Microphone)) -->|Int16 PCM 48kHz| Capture[AudioCapture]
    
    subgraph "AudioPipeline (Rust Core)"
        Capture -->|Input Frame (960 samples)| Opus[Opus Encoder]
        Opus -->|Compressed Bytes| Crypto[XChaCha20-Poly1305]
        Crypto -->|Ciphertext| Header[FastHeader Builder]
        Header -->|Add Nonce/Seq/SessionID| Packet[FastAudioPacket]
    end
    
    Packet -->|Bytes| Network[QUIC Stream/Datagram]
    
    Network -->|Bytes| ServerRelay{Server Relay}
    ServerRelay -->|Bytes| Recipient[Recipient Client]
    
    subgraph "Receiver Pipeline (Future)"
        Recipient -->|Parse Header| Decrypt[Decrypt]
        Decrypt -->|Jitter Buffer| Jitter[Reorder & Buffer]
        Jitter -->|Opus Frame| Decode[Opus Decoder]
        Decode -->|PCM| Mixer[Audio Mixer]
    end
    
    Mixer -->|Float PCM| Speakers((Speakers))
```

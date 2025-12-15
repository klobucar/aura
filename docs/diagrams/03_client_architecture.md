# Client Architecture

This diagram illustrates the architecture of the macOS Swift Client and its interaction with the Rust Core.

```mermaid
graph TD
    subgraph UI [SwiftUI Layer]
        ContentView --> ConnectionStatus
        ContentView --> ChannelView
    end

    subgraph Transport [Network Layer]
        QNC[QuicNetworkClient]
        QNC -->|NWConnectionGroup| Network.framework
        QNC -->|Authentication| AuthService[Auth Logic]
    end

    subgraph Audio [Audio Subsystem]
        Capture[AudioCapture (AVFoundation)]
        Pipeline[AudioPipeline (Swift Wrapper)]
        Play[AudioPlayback (TODO)]
    end

    subgraph RustCore [Rust Core (UniFFI)]
        RS[AudioSender]
        RR[AudioReceiver]
        Opus[OpusCodec]
        Crypto[DaveCrypto]
    end

    ContentView -->|Connect/Join| QNC
    QNC -->|Initialize| Pipeline
    
    Capture -->|PCM 48kHz| Pipeline
    Pipeline -->|Process| RS
    RS -->|Encode| Opus
    RS -->|Encrypt| Crypto
    RS -->|Packet Bytes| Pipeline
    Pipeline -->|Send| QNC
    QNC -->|Send 0x20| Server
```

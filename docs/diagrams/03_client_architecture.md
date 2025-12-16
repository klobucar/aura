# Client Architecture

This diagram illustrates the architecture of the macOS Swift Client and its interaction with the Rust Core via UniFFI.

```mermaid
graph TB
    subgraph UI ["SwiftUI Layer"]
        CV["ContentView"]
        CV -->|Display| CS["Connection Status"]
        CV -->|Display| CL["Channel List"]
        CV -->|Display| CHAT["Chat Messages"]
        CV -->|Show indicators| TI["Talking Indicators<br/>(Green dots)"]
    end

    subgraph Services ["Swift Services Layer"]
        QNC["QuicNetworkClient<br/>(Network.framework)"]
        AC["AudioCapture<br/>(AVAudioEngine)"]
        AP["AudioPlayback<br/>(AVAudioEngine)"]
        ID["UserIdentity<br/>(Keychain)"]
    end

    subgraph RustCore ["Rust Core (UniFFI Bindings)"]
        direction TB
        ASW["AudioSenderWrapper"]
        ARW["AudioReceiverWrapper"]
        TC["TextCryptoWrapper"]
        
        subgraph Pipeline ["Audio Pipeline"]
            OPUS["Opus Codec"]
            DAVE["DAVE Crypto<br/>(XChaCha20-Poly1305)"]
            JB["Jitter Buffer"]
        end
    end

    subgraph Network ["Network Layer"]
        QUIC["QUIC Connection<br/>(NWConnectionGroup)"]
        CS_STREAM["Control Stream<br/>(Reliable)"]
        DG["Datagrams<br/>(Unreliable)"]
    end

    %% UI to Services
    CV -->|"Connect/Join"| QNC
    CV -->|"Start PTT"| AC
    CV -->|"Send Message"| QNC

    %% Services to Rust
    AC -->|"PCM Data"| ASW
    ASW -->|"process()"| OPUS
    OPUS --> DAVE
    DAVE -->|"Encrypted bytes"| QNC

    QNC -->|"Received packets"| ARW
    ARW --> DAVE
    DAVE --> OPUS
    OPUS --> JB
    JB -->|"popMixed()"| AP

    %% Talking Indicators
    ARW -.->|"popDecoded()"| QNC
    QNC -.->|"activeSpeakers Set"| TI

    %% Text
    QNC -->|"Encrypt/Decrypt"| TC

    %% Network
    QNC --> QUIC
    QUIC --> CS_STREAM
    QUIC --> DG

    %% Authentication
    ID -->|"Sign Challenge"| QNC

    %% Server
    QUIC -->|"TLS 1.3"| SERVER["Aura Server<br/>(Rust)"]

    style UI fill:#e1f5ff
    style Services fill:#fff9e1
    style RustCore fill:#e1e5ff
    style Network fill:#ffe1f5
    style SERVER fill:#ffe1e1
```

## Component Responsibilities

### SwiftUI Layer
- **ContentView** - Main UI, displays channels, users, chat, and talking indicators
- Observes `QuicNetworkClient` state for reactive updates

### Swift Services
- **QuicNetworkClient** - Manages QUIC connection, auth, channel join, message routing
  - Tracks `activeSpeakers: Set<UInt32>` for talking indicators
  - Handles transmit (`sendAudioDatagram`) and receive (`handleAudioPacket`)
- **AudioCapture** - Captures 48kHz mono PCM from microphone
- **AudioPlayback** - Plays mixed audio via AVAudioEngine
- **UserIdentity** - Manages Ed25519 keypair in Keychain

### Rust Core (UniFFI)
- **AudioSenderWrapper** - Encodes (Opus) + encrypts (DAVE) outgoing audio
- **AudioReceiverWrapper** - Decrypts + decodes + buffers incoming audio
  - Provides `popMixed()` for mixed audio from all senders
  - Provides `popDecoded()` for per-sender frames (talking indicators)
- **TextCryptoWrapper** - Encrypts/decrypts text messages

### Network Layer
- **QUIC** - TLS 1.3 encrypted transport
- **Control Stream** - Reliable ordered messages (auth, join, text, audio)
- **Datagrams** - Unreliable low-latency (future: audio packets)

## Data Flows

### Voice Transmit (Alice → Server → Bob)
1. Mic → AudioCapture → PCM Data
2. PCM → AudioSenderWrapper → Encrypted Packet
3. Packet → QuicNetworkClient → Control Stream (0x20)
4. Server relays to Bob's QuicNetworkClient
5. Bob's QuicNetworkClient → AudioReceiverWrapper → Decrypt/Decode
6. AudioReceiverWrapper.popMixed() → AudioPlayback → Speakers

### Talking Indicators
- AudioReceiverWrapper.popDecoded() returns `DecodedFrame[]` with `sessionId`
- QuicNetworkClient adds `sessionId` to `activeSpeakers` Set
- UI displays green dot next to users in `activeSpeakers`
- Expire after ~300ms of silence (timer in UI)

### Text Message Flow (Encrypted)

```mermaid
sequenceDiagram
    participant UI as SwiftUI View
    participant Client as QuicNetworkClient
    participant Server as Aura Server
    participant Peer as Peer Client

    UI->>Client: sendText("Hello")
    Note right of UI: Gen msg_UUIDv4 (Optimistic)
    
    Client->>Client: Encrypt (XChaCha20)
    Client->>Server: TEXT Packet [msg_ID][Ciphertext]
    
    Server->>Server: Relay (Zero-Knowledge)
    Server-->>Client: Echo [msg_ID][Ciphertext]
    Server-->>Peer: Relay [msg_ID][Ciphertext]
    
    Peer->>Peer: Decrypt & Display
    Client->>Client: Match Echo by msg_ID (Dedupe)
```

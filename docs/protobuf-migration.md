# Aura Protobuf Migration Proposal

## 1. Problem Statement
The current Aura Network Protocol uses a custom manual binary format for all message types (Auth, Control, Text, Audio).
While optimal for the minimal overhead required by **Audio** (the "Hot Path"), this approach is brittle and limiting for **Text and Control** messages (the "Cold Path").

**Current Limitations:**
*   **Fragility**: Manual byte parsing (slicing arrays, managing offsets) is error-prone.
*   **Rigidity**: Adding features like File Attachments (Base64 images), Rich Text, or Reply Threading requires changing the wire format, breaking compatibility.
*   **Duplication**: Logic for serialization must be manually re-written in Rust, Swift, and C#.

## 2. Proposed Architecture: Hybrid Protocol
We propose a **Hybrid Protocol** that leverages the strengths of both formats:

| Traffic Type | Message Frequency | Transport Format | Justification |
| :--- | :--- | :--- | :--- |
| **Audio** (Media) | High (50pps/user) | **Custom Binary** (`FastAudioPacket`) | Requires Zero-Copy routing and minimal CPU overhead. **Keep as is.** |
| **Control** | Low (Events) | **Protobuf** | Join/Leave/State events are complex but infrequent. Structuring them prevents state desync. |
| **Chat** | Variable | **Protobuf** | Text messages need rich structure (Attachments, Replies). Overhead of Protobuf is negligible here. |

## 3. Schema Design (`aura.proto`)
We will standardize all "Cold Path" messages under a unified `ControlEnvelope` or specific opcodes.

### Proposed `aura.proto` Additions

```protobuf
syntax = "proto3";
package aura.v1;

// Universal Container for Control Stream
message ControlEnvelope {
    // Request ID for correlation (optional)
    string request_id = 1;
    
    oneof payload {
        AuthRequest auth = 10;
        JoinChannel join = 11;
        TextMessage text = 12;
        
        // Server -> Client Events
        ChannelState channel_state = 20;
        UserEvent user_event = 21;
    }
}

message TextMessage {
    uint32 channel_id = 1;
    string content = 2; // Plain text
    
    // Future-proofing features
    optional string reply_to_id = 3;
    repeated Attachment attachments = 4;
}

message Attachment {
    string filename = 1;
    string mime_type = 2;
    bytes data = 3; // Image/File data (Chunking recommended for large files)
}

message JoinChannel {
    uint32 channel_id = 1;
    string password = 2; // Support for private channels
}
```

## 4. Implementation Plan

### Phase 1: Server Updates (`crates/aura-server`)
1.  Add `prost` dependency (if not present) for Protobuf support.
2.  Introduce a new behaviors for Opcode `0x40` (Protobuf Control Message).
3.  Implement `handle_control_envelope` to dispatch based on the `oneof` field.
4.  Maintain `0x20` (Audio) handler as the optimized Hot Path.

### Phase 2: Client Migration
1.  **Shared**: Generate Code from `aura.proto` for Rust, C#, and Swift.
2.  **C# Client**:
    *   Leverage `Grpc.Tools`.
    *   Refactor `SendTextMessageAsync` to serialize `ControlEnvelope`.
3.  **Swift Client**:
    *   Add `SwiftProtobuf` package.
    *   Refactor `QuicNetworkClient` to handle `ControlEnvelope`.

### Phase 3: Deprecation
1.  Mark Opcode `0x10` (Legacy Join) and `0x30` (Legacy Text) as deprecated.
2.  Remove legacy handlers in a future major version update.

## 5. Value Add
*   **File Sharing**: Enable sending images directly in chat using `bytes` fields.
*   **Strict Contracts**: The `.proto` file becomes the single source of truth for the API.
*   **Safety**: Eliminate buffer overflow/underflow risks associated with manual binary interactions.

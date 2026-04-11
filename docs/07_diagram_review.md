# Architectural Diagram Review: Security Scrutiny

**Reviewer**: Antigravity (Senior Security Architect)  
**Subject**: Evaluation of `docs/diagrams/` for protocol accuracy.

I have scrutinized the four core architectural diagrams against our recent security enhancements (MLS Per-Sender Keys, Epoch Handover, Replay Protection).

## 1. General Assessment
The diagrams are **90% accurate** and demonstrate a strong understanding of the "QUIC-Native" philosophy. The choice of XChaCha20 and the clear separation between Control Streams and Datagrams are correctly depicted.

---

## 2. Specific Findings & Recommendations

### [01_protocol_flow.md](file:///Users/crabclaw/src/aura/docs/diagrams/01_protocol_flow.md)
*   **Accuracy Status**: Correct on transport, incomplete on security.
*   **Scrutiny**: The diagram shows a generic "Encrypt (DAVE)" step. To reflect our recent hardening, it should explicitly show that encryption uses a **Sender-ID-bound Key**.
*   **Recommendation**: 
    - Update Line 33: `Encrypt (DAVE: Per-Sender Key)`
    - Add a note about **MLS Epoch Handover** during the "Channel Join" phase to clarify how Bob knows which key Alice is using.

### [03_client_architecture.md](file:///Users/crabclaw/src/aura/docs/diagrams/03_client_architecture.md)
*   **Accuracy Status**: Minor inconsistency found.
*   **Scrutiny**: Line 114 says "Packet -> QuicNetworkClient -> **Control Stream**". This contradicts the "Datagrams" label in the diagram and the performance goals of the project. Audio should never traverse the Control Stream.
*   **Recommendation**:
    - Update Line 114: `Packet -> QuicNetworkClient -> QUIC Datagram (Opcode 0x01)`
    - Add `Epoch Store` to the `Rust Core` subgraph to show where the 3 active keys are held.

### [04_audio_pipeline.md](file:///Users/crabclaw/src/aura/docs/diagrams/04_audio_pipeline.md)
*   **Accuracy Status**: Highly accurate, needs a metadata update.
*   **Scrutiny**: Line 76 still says "currently using session token for PoC". This is no longer true as we have fully implemented per-sender MLS secret exports.
*   **Recommendation**:
    - Update Line 76: `Key: Derived from MLS export_secret using sender_id context (Per-sender isolation).`
    - Add "Replay Guard" to the `RustRx` subgraph (Line 29/30) to show where sequence numbers are checked.

---

## 3. Summary of "Scrutiny Score"

| Diagram | Clarity | Security Accuracy | Alignment with Implementation |
| :--- | :--- | :--- | :--- |
| **01 Flow** | High | Medium (Missing MLS) | High |
| **02 Server** | High | High (Relay only) | High |
| **03 Client** | High | High | High (one typo found) |
| **04 Pipeline**| Excellent| High | High (metadata outdated) |

## Final Word
The diagrams reflect a robust design. By making these minor textual updates to reflect our shift from "PoC" to "Production Security," they will serve as authoritative documentation for the Aura core.

//! Relay for end-to-end encrypted file transfers. See
//! `docs/protocol/file_transfer.md` for the wire format and design.
//!
//! A sender opens a client-initiated unidirectional QUIC stream per file and
//! writes a [`FileStreamHeader`] followed by length-prefixed ciphertext
//! chunks. The relay checks the header, checks the sender is in the channel,
//! opens a server-initiated unidirectional stream to every *other* current
//! member, and forwards header and chunks as they arrive.
//!
//! The relay never decrypts, never writes to disk or the database and never
//! holds more than a small bounded window of a file in memory: each recipient
//! gets a bounded queue of `Bytes` (shared, not copied, between recipients).
//! A recipient whose queue fills loses its copy; the sender and the other
//! recipients carry on. Nothing about a file (name, type, content) is logged.

use crate::rate_limit::FileByteLimiter;
use crate::state::ServerState;
use aura_protocol::file_transfer::{
    error_code as code, FileHeaderError, FileStreamHeader, FILE_HEADER_FIXED_LEN,
    FILE_MAX_CHUNK_CIPHERTEXT,
};
use bytes::{BufMut, Bytes, BytesMut};
use dashmap::DashMap;
use quinn::{Connection, RecvStream, SendStream, VarInt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tracing::{debug, info};

/// Transfers one user may have running at once (as sender).
pub const MAX_CONCURRENT_TRANSFERS_PER_USER: u32 = 3;
/// Ciphertext one user may start sending per minute: 100 MiB. Charged when a
/// transfer starts, by its declared size (at least one chunk).
pub const MAX_TRANSFER_KIB_PER_MINUTE: u32 = 100 * 1024;
/// Chunks queued per recipient before its copy is dropped (64 x 64 KiB = 4 MiB).
pub const RECIPIENT_BUFFER_CHUNKS: usize = 64;

/// Tunables, split out so tests can shrink them.
#[derive(Debug, Clone)]
pub struct RelayLimits {
    pub max_concurrent_per_user: u32,
    pub kib_per_minute_per_user: u32,
    pub recipient_buffer_chunks: usize,
    /// Time allowed for the sender's header to arrive.
    pub header_timeout: Duration,
    /// Longest the sender may go without delivering a complete chunk.
    pub chunk_idle_timeout: Duration,
    /// Longest one write to a recipient may block.
    pub recipient_write_timeout: Duration,
    /// Time allowed to open a stream to a recipient.
    pub open_stream_timeout: Duration,
    /// Wall-clock cap on one transfer.
    pub max_transfer_duration: Duration,
    /// Once the sender has finished, how long recipients get to drain what is
    /// queued for them. Stragglers are then cut off, so a recipient that reads
    /// slowly on purpose can't hold the sender's transfer slot.
    pub drain_timeout: Duration,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            max_concurrent_per_user: MAX_CONCURRENT_TRANSFERS_PER_USER,
            kib_per_minute_per_user: MAX_TRANSFER_KIB_PER_MINUTE,
            recipient_buffer_chunks: RECIPIENT_BUFFER_CHUNKS,
            header_timeout: Duration::from_secs(10),
            chunk_idle_timeout: Duration::from_secs(30),
            recipient_write_timeout: Duration::from_secs(30),
            open_stream_timeout: Duration::from_secs(5),
            max_transfer_duration: Duration::from_secs(15 * 60),
            drain_timeout: Duration::from_secs(120),
        }
    }
}

struct ActiveTransfer {
    sender_session: u32,
    abort: watch::Sender<u32>,
}

/// Connection registry and transfer bookkeeping. One per [`ServerState`].
pub struct FileRelay {
    conns: DashMap<u32, Connection>,
    active: DashMap<u64, ActiveTransfer>,
    per_user: DashMap<String, u32>,
    next_id: AtomicU64,
    bytes: FileByteLimiter,
    limits: RelayLimits,
}

impl Default for FileRelay {
    fn default() -> Self {
        Self::with_limits(RelayLimits::default())
    }
}

/// Holds one of a user's concurrent-transfer slots.
struct SlotGuard {
    state: Arc<ServerState>,
    user: String,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let relay = &self.state.file_relay;
        if let Some(mut n) = relay.per_user.get_mut(&self.user) {
            *n = n.saturating_sub(1);
        }
        relay.per_user.remove_if(&self.user, |_, n| *n == 0);
    }
}

/// Removes a transfer from the active table when it ends, however it ends.
struct ActiveGuard {
    state: Arc<ServerState>,
    id: u64,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.state.file_relay.active.remove(&self.id);
    }
}

impl FileRelay {
    pub fn with_limits(limits: RelayLimits) -> Self {
        Self {
            conns: DashMap::new(),
            active: DashMap::new(),
            per_user: DashMap::new(),
            next_id: AtomicU64::new(1),
            bytes: FileByteLimiter::new(limits.kib_per_minute_per_user),
            limits,
        }
    }

    /// Make a session's connection reachable for deliveries.
    pub fn register(&self, session_id: u32, conn: Connection) {
        self.conns.insert(session_id, conn);
    }

    /// Forget a session's connection and abort any transfer it is sending.
    pub fn unregister(&self, session_id: u32) {
        self.conns.remove(&session_id);
        self.abort_sender(session_id);
    }

    /// Abort every transfer `session_id` is sending (it left its channel or
    /// went away). Recipients see a reset with `SENDER_LEFT`.
    pub fn abort_sender(&self, session_id: u32) {
        for t in self.active.iter() {
            if t.sender_session == session_id {
                t.abort.send_replace(code::SENDER_LEFT);
            }
        }
    }

    /// Transfers currently being relayed.
    pub fn active_transfers(&self) -> usize {
        self.active.len()
    }

    fn try_acquire_slot(&self, state: &Arc<ServerState>, user: &str) -> Option<SlotGuard> {
        let mut n = self.per_user.entry(user.to_string()).or_insert(0);
        if *n >= self.limits.max_concurrent_per_user {
            return None;
        }
        *n += 1;
        Some(SlotGuard {
            state: Arc::clone(state),
            user: user.to_string(),
        })
    }
}

/// Why a transfer ended early: the QUIC code sent to both sides and the
/// metrics label for the outcome.
struct Abort {
    code: u32,
    outcome: &'static str,
}

impl Abort {
    const fn new(code: u32, outcome: &'static str) -> Self {
        Self { code, outcome }
    }
}

fn varint(code: u32) -> VarInt {
    VarInt::from_u32(code)
}

/// One recipient's end of the fan-out.
struct Recipient {
    session_id: u32,
    tx: mpsc::Sender<Bytes>,
    /// Set to a non-zero code when the relay cuts this recipient off, so its
    /// task resets the stream with that code.
    cut: watch::Sender<u32>,
}

/// Session ids currently in the channel's text group, or `None` if the
/// channel has no group.
async fn channel_members(state: &ServerState, channel_id: &str) -> Option<Vec<u32>> {
    let group_id = aura_protocol::make_mls_group_id(channel_id, false);
    let group = state.text_groups.get(&group_id)?.clone();
    let group = group.read().await;
    Some(group.members.iter().map(|m| *m).collect())
}

async fn read_header(recv: &mut RecvStream, limits: &RelayLimits) -> Result<FileStreamHeader, u32> {
    let read = async {
        let mut fixed = [0u8; FILE_HEADER_FIXED_LEN];
        recv.read_exact(&mut fixed).await.ok()?;
        let ch_len = FileStreamHeader::channel_id_len_from_prefix(&fixed).ok()?;
        let mut full = fixed.to_vec();
        full.resize(FILE_HEADER_FIXED_LEN + ch_len, 0);
        recv.read_exact(&mut full[FILE_HEADER_FIXED_LEN..])
            .await
            .ok()?;
        Some(FileStreamHeader::decode(&full))
    };
    match timeout(limits.header_timeout, read).await {
        Ok(Some(Ok(h))) => Ok(h),
        Ok(Some(Err(FileHeaderError::TooLarge))) => Err(code::TOO_LARGE),
        _ => Err(code::MALFORMED_HEADER),
    }
}

/// Handle one client-opened bidirectional stream: the whole life of an
/// upload. When it ends the server writes the sender an 8-byte status
/// (`[u32 LE code][u32 LE recipients]`, code 0 = accepted and relayed to
/// `recipients` members) and finishes its side. Never returns an error.
pub async fn handle_upload(
    state: Arc<ServerState>,
    session_id: u32,
    user_uuid: String,
    mut send: SendStream,
    mut recv: RecvStream,
) {
    let (status, recipients) = match upload(&state, session_id, &user_uuid, &mut recv).await {
        Ok(n) => (0, n),
        Err(c) => {
            let _ = recv.stop(varint(c));
            (c, 0)
        }
    };
    let mut reply = [0u8; 8];
    reply[..4].copy_from_slice(&status.to_le_bytes());
    reply[4..].copy_from_slice(&recipients.to_le_bytes());
    // Best effort: the sender may already be gone.
    if send.write_all(&reply).await.is_ok() {
        let _ = send.finish();
    }
}

/// The upload proper. `Ok(n)`: relayed to `n` recipients. `Err(code)`: refused
/// or aborted for that reason.
async fn upload(
    state: &Arc<ServerState>,
    session_id: u32,
    user_uuid: &str,
    recv: &mut RecvStream,
) -> Result<u32, u32> {
    let relay = &state.file_relay;
    let limits = relay.limits.clone();
    let metrics = &crate::metrics::METRICS;

    let reject = |c: u32, reason: &str| -> u32 {
        metrics.file_transfer_rejected(reason);
        c
    };

    let header = match read_header(recv, &limits).await {
        Ok(h) => h,
        Err(c) => {
            return Err(reject(
                c,
                if c == code::TOO_LARGE {
                    "too_large"
                } else {
                    "malformed"
                },
            ));
        }
    };

    // The sender must be in the channel right now.
    let members = match channel_members(state, &header.channel_id).await {
        Some(m) if m.contains(&session_id) => m,
        _ => {
            return Err(reject(code::NOT_IN_CHANNEL, "not_in_channel"));
        }
    };

    // Rate limits: concurrent slots, then the byte budget.
    let Some(slot) = relay.try_acquire_slot(state, user_uuid) else {
        return Err(reject(code::RATE_LIMITED, "rate_limited"));
    };
    if !relay
        .bytes
        .try_consume(user_uuid, header.total_ciphertext_len)
    {
        return Err(reject(code::RATE_LIMITED, "rate_limited"));
    }

    // Recipients: everyone else in the channel right now who is connected.
    let targets: Vec<(u32, Connection)> = members
        .into_iter()
        .filter(|m| *m != session_id)
        .filter_map(|m| relay.conns.get(&m).map(|c| (m, c.clone())))
        .collect();
    if targets.is_empty() {
        return Err(reject(code::NO_RECIPIENTS, "no_recipients"));
    }

    let transfer_id = relay.next_id.fetch_add(1, Ordering::Relaxed);
    let (abort_tx, abort_rx) = watch::channel(0u32);
    relay.active.insert(
        transfer_id,
        ActiveTransfer {
            sender_session: session_id,
            abort: abort_tx.clone(),
        },
    );
    let _active = ActiveGuard {
        state: Arc::clone(state),
        id: transfer_id,
    };
    metrics.file_transfer_started();
    debug!(
        "[files] transfer {} started: {} chunks, {} bytes, {} recipients",
        transfer_id,
        header.chunk_count,
        header.total_ciphertext_len,
        targets.len()
    );

    // Deliveries carry the real sender session id.
    let mut out_header = header.clone();
    out_header.sender_session_id = session_id;
    let header_bytes = Bytes::from(out_header.encode());

    let mut recipients = Vec::with_capacity(targets.len());
    let mut tasks = Vec::with_capacity(targets.len());
    for (rsession, conn) in targets {
        let (tx, rx) = mpsc::channel(limits.recipient_buffer_chunks.max(1));
        let (cut, cut_rx) = watch::channel(0u32);
        tasks.push(tokio::spawn(deliver(
            conn,
            header_bytes.clone(),
            rx,
            cut_rx,
            abort_rx.clone(),
            limits.clone(),
        )));
        recipients.push(Recipient {
            session_id: rsession,
            tx,
            cut,
        });
    }

    let result = timeout(
        limits.max_transfer_duration,
        pump(
            state,
            &header,
            session_id,
            recv,
            &mut recipients,
            abort_rx,
            &limits,
        ),
    )
    .await
    .unwrap_or(Err(Abort::new(code::SENDER_STALLED, "sender_stalled")));

    match result {
        Ok(()) => {
            // Closing every queue is the clean end: recipients drain what is
            // queued and finish their streams. Keep the cut-off switches.
            let cuts: Vec<watch::Sender<u32>> = recipients
                .drain(..)
                .map(|Recipient { cut, .. }| cut)
                .collect();
            // Hold the slot until they have, so a user's server-side buffering
            // stays bounded by the slot count, but only for `drain_timeout`:
            // after that, whoever is still writing is cut off.
            let deadline = tokio::time::Instant::now() + limits.drain_timeout;
            let mut delivered = 0u32;
            let mut cut_stragglers = false;
            for mut t in tasks {
                let done = match tokio::time::timeout_at(deadline, &mut t).await {
                    Ok(r) => r,
                    Err(_) => {
                        if !cut_stragglers {
                            cut_stragglers = true;
                            for c in &cuts {
                                c.send_replace(code::RECIPIENT_TOO_SLOW);
                            }
                        }
                        metrics.file_recipient_dropped();
                        t.await
                    }
                };
                if matches!(done, Ok(true)) {
                    delivered += 1;
                }
            }
            metrics.file_transfer_finished("completed");
            debug!("[files] transfer {} completed", transfer_id);
            drop(slot);
            Ok(delivered)
        }
        Err(abort) => {
            // Signal before the queues close, or a recipient could mistake the
            // closure for a clean end.
            abort_tx.send_replace(abort.code);
            drop(recipients);
            for t in tasks {
                let _ = t.await;
            }
            metrics.file_transfer_finished(abort.outcome);
            info!(
                "[files] transfer {} aborted: {} (code {})",
                transfer_id, abort.outcome, abort.code
            );
            drop(slot);
            Err(abort.code)
        }
    }
}

/// Read chunks from the sender and fan them out. `Ok` means the sender
/// delivered exactly what its header declared and finished the stream.
async fn pump(
    state: &Arc<ServerState>,
    header: &FileStreamHeader,
    session_id: u32,
    recv: &mut RecvStream,
    recipients: &mut Vec<Recipient>,
    mut abort: watch::Receiver<u32>,
    limits: &RelayLimits,
) -> Result<(), Abort> {
    let metrics = &crate::metrics::METRICS;
    let gone = |e: u32| -> Abort {
        match e {
            code::SENDER_LEFT => Abort::new(code::SENDER_LEFT, "sender_left"),
            other => Abort::new(other, "other"),
        }
    };

    for index in 0..header.chunk_count {
        // Sender still in the channel? Recipients who left stop receiving.
        let members = channel_members(state, &header.channel_id)
            .await
            .unwrap_or_default();
        if !members.contains(&session_id) {
            return Err(Abort::new(code::SENDER_LEFT, "sender_left"));
        }
        recipients.retain(|r| {
            let keep = members.contains(&r.session_id);
            if !keep {
                r.cut.send_replace(code::NOT_IN_CHANNEL);
            }
            keep
        });
        if recipients.is_empty() {
            return Err(Abort::new(code::NO_RECIPIENTS, "recipients_gone"));
        }

        let expected = header
            .chunk_ciphertext_len(index)
            .expect("index below chunk_count");
        let read = async {
            let mut len_buf = [0u8; 4];
            recv.read_exact(&mut len_buf).await.ok()?;
            let len = u32::from_le_bytes(len_buf) as usize;
            if len != expected {
                return Some(Err(()));
            }
            debug_assert!(len <= FILE_MAX_CHUNK_CIPHERTEXT);
            let mut buf = BytesMut::with_capacity(4 + len);
            buf.put_slice(&len_buf);
            buf.resize(4 + len, 0);
            recv.read_exact(&mut buf[4..]).await.ok()?;
            Some(Ok(buf.freeze()))
        };
        let chunk = tokio::select! {
            biased;
            c = signalled(&mut abort) => return Err(gone(c)),
            r = timeout(limits.chunk_idle_timeout, read) => match r {
                Err(_) => return Err(Abort::new(code::SENDER_STALLED, "sender_stalled")),
                // Stream reset or connection lost mid-chunk.
                Ok(None) => return Err(Abort::new(code::SENDER_LEFT, "sender_left")),
                Ok(Some(Err(()))) => {
                    return Err(Abort::new(code::PROTOCOL_VIOLATION, "protocol_violation"))
                }
                Ok(Some(Ok(b))) => b,
            },
        };
        metrics.file_bytes_received(chunk.len() as u64 - 4);

        recipients.retain(|r| match r.tx.try_send(chunk.clone()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                r.cut.send_replace(code::RECIPIENT_TOO_SLOW);
                metrics.file_recipient_dropped();
                false
            }
            // Its task ended (connection gone or write failed).
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        });
        if recipients.is_empty() {
            return Err(Abort::new(code::NO_RECIPIENTS, "recipients_gone"));
        }
    }

    // The sender must now finish the stream. Anything more is a violation,
    // caught before any recipient is told the file is complete.
    let mut probe = [0u8; 1];
    match timeout(limits.chunk_idle_timeout, recv.read(&mut probe)).await {
        Ok(Ok(None)) => Ok(()),
        Ok(Ok(Some(_))) => Err(Abort::new(code::PROTOCOL_VIOLATION, "protocol_violation")),
        Ok(Err(_)) => Err(Abort::new(code::SENDER_LEFT, "sender_left")),
        Err(_) => Err(Abort::new(code::SENDER_STALLED, "sender_stalled")),
    }
}

/// Resolves with the code once `rx` is set non-zero; never if the sender goes
/// away without setting one (a clean end drops the senders).
async fn signalled(rx: &mut watch::Receiver<u32>) -> u32 {
    loop {
        if rx.changed().await.is_err() {
            return std::future::pending().await;
        }
        let v = *rx.borrow();
        if v != 0 {
            return v;
        }
    }
}

/// Write `data` to a recipient, giving up early if the transfer is aborted or
/// this recipient is cut off. `Err` carries the code to reset the stream with.
async fn write_or_stop(
    send: &mut SendStream,
    data: &[u8],
    abort: &mut watch::Receiver<u32>,
    cut: &mut watch::Receiver<u32>,
    limit: Duration,
) -> Result<(), u32> {
    tokio::select! {
        biased;
        c = signalled(abort) => Err(c),
        c = signalled(cut) => Err(c),
        r = timeout(limit, send.write_all(data)) => match r {
            Ok(Ok(())) => Ok(()),
            // Stuck or broken: cut this copy off.
            _ => Err(code::RECIPIENT_TOO_SLOW),
        },
    }
}

/// One recipient's task: open a stream, write the header, then forward queued
/// chunks until the queue closes or the transfer is aborted. Returns whether
/// this recipient's copy was delivered in full.
async fn deliver(
    conn: Connection,
    header: Bytes,
    mut rx: mpsc::Receiver<Bytes>,
    mut cut: watch::Receiver<u32>,
    mut abort: watch::Receiver<u32>,
    limits: RelayLimits,
) -> bool {
    let metrics = &crate::metrics::METRICS;
    let mut send = match timeout(limits.open_stream_timeout, conn.open_uni()).await {
        Ok(Ok(s)) => s,
        _ => return false,
    };
    let limit = limits.recipient_write_timeout;

    let outcome: Result<(), u32> = async {
        write_or_stop(&mut send, &header, &mut abort, &mut cut, limit).await?;
        loop {
            let chunk = tokio::select! {
                biased;
                c = signalled(&mut abort) => return Err(c),
                c = signalled(&mut cut) => return Err(c),
                c = rx.recv() => c,
            };
            let Some(chunk) = chunk else { return Ok(()) };
            write_or_stop(&mut send, &chunk, &mut abort, &mut cut, limit).await?;
            metrics.file_bytes_relayed(chunk.len() as u64 - 4);
        }
    }
    .await;

    match outcome {
        Ok(()) => send.finish().is_ok(),
        Err(c) => {
            let _ = send.reset(varint(c));
            false
        }
    }
}

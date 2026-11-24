//! Rate limiters.
//!
//! [`HandshakeRateLimiter`] sits in front of the QUIC accept loop so an
//! attacker cannot burn Ed25519 signature-verification CPU (or TLS handshake
//! CPU) by replaying bogus `AuthRequest` packets from a single address.
//! Loopback traffic is always allowed so local tests and same-host tooling
//! are unaffected.
//!
//! [`PingRateLimiter`] caps how often one connection's control-stream pings
//! are answered.

use governor::{
    clock::DefaultClock, state::keyed::DashMapStateStore, DefaultDirectRateLimiter, Quota,
    RateLimiter,
};
use nonzero_ext::nonzero;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

type KeyedLimiter = RateLimiter<IpAddr, DashMapStateStore<IpAddr>, DefaultClock>;

/// Rejection reason returned when a peer exceeds its quota.
#[derive(Debug, thiserror::Error)]
#[error("handshake rate limit exceeded for {ip}")]
pub struct RateLimitExceeded {
    pub ip: IpAddr,
}

/// Per-IP token-bucket limiter for QUIC handshake attempts.
#[derive(Clone)]
pub struct HandshakeRateLimiter {
    inner: Arc<KeyedLimiter>,
}

impl HandshakeRateLimiter {
    /// Build a limiter allowing `per_minute` attempts per IP with the given
    /// burst capacity. A background task periodically prunes idle entries
    /// so a flood of unique source IPs cannot exhaust memory.
    pub fn new(per_minute: u32, burst: u32) -> Self {
        let per_minute = NonZeroU32::new(per_minute).unwrap_or(nonzero!(60u32));
        let burst = NonZeroU32::new(burst).unwrap_or(nonzero!(20u32));
        let quota = Quota::per_minute(per_minute).allow_burst(burst);

        let inner = Arc::new(RateLimiter::dashmap(quota));
        let gc_handle = Arc::clone(&inner);

        // Prune entries that have fully refilled so the keyed map does
        // not grow without bound under a spoofed-source flood.
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            ticker.tick().await; // discard the immediate first tick
            loop {
                ticker.tick().await;
                gc_handle.retain_recent();
                debug!(
                    "[RateLimit] GC sweep complete, {} active IPs tracked",
                    gc_handle.len()
                );
            }
        });

        info!(
            "[RateLimit] Handshake limiter: {}/min per IP, burst {}",
            per_minute, burst
        );

        Self { inner }
    }

    /// Check whether `ip` may proceed with a handshake attempt.
    /// Loopback is always permitted.
    pub fn check(&self, ip: IpAddr) -> Result<(), RateLimitExceeded> {
        if ip.is_loopback() {
            return Ok(());
        }
        match self.inner.check_key(&ip) {
            Ok(()) => Ok(()),
            Err(_) => Err(RateLimitExceeded { ip }),
        }
    }
}

/// Per-connection token bucket for control-stream pings (`MSG_PING`).
///
/// Clients probe every few seconds; a burst of 5 covers a client that probes
/// a few times on connect, and 2 per second sustained is far above any real
/// probe rate. Pings over the limit are read and dropped unanswered, which
/// the client sees as a lost probe; the connection stays up.
pub struct PingRateLimiter {
    inner: DefaultDirectRateLimiter,
}

impl Default for PingRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl PingRateLimiter {
    pub fn new() -> Self {
        let quota = Quota::per_second(nonzero!(2u32)).allow_burst(nonzero!(5u32));
        Self {
            inner: RateLimiter::direct(quota),
        }
    }

    /// Whether this ping may be answered. Uses up a token if so.
    pub fn check(&self) -> bool {
        self.inner.check().is_ok()
    }
}

/// Per-user budget of file-transfer ciphertext per minute.
///
/// Charged when a transfer starts, by its declared size in KiB and never less
/// than one chunk, so a flood of tiny transfers is limited too. The budget is
/// also the burst, so a user can spend a whole minute's worth at once.
pub struct FileByteLimiter {
    inner: governor::DefaultKeyedRateLimiter<String>,
    checks: std::sync::atomic::AtomicU32,
}

impl FileByteLimiter {
    /// Smallest charge per transfer, in KiB (one 64 KiB chunk).
    const MIN_CHARGE_KIB: u64 = 64;

    pub fn new(kib_per_minute: u32) -> Self {
        let kib = NonZeroU32::new(kib_per_minute).unwrap_or(nonzero!(1u32));
        Self {
            inner: RateLimiter::keyed(Quota::per_minute(kib).allow_burst(kib)),
            checks: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Spend `bytes` of `user`'s budget. False if it would overdraw it.
    pub fn try_consume(&self, user: &str, bytes: u64) -> bool {
        // Prune fully refilled entries now and then so the map follows the
        // number of recently active senders, not all users ever seen.
        if self
            .checks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % 256
            == 255
        {
            self.inner.retain_recent();
        }
        let kib = bytes.div_ceil(1024).max(Self::MIN_CHARGE_KIB);
        let Some(cells) = u32::try_from(kib).ok().and_then(NonZeroU32::new) else {
            return false;
        };
        matches!(self.inner.check_key_n(&user.to_string(), cells), Ok(Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn ping_limiter_allows_a_burst_then_refuses() {
        let limiter = PingRateLimiter::new();
        let answered = (0..20).filter(|_| limiter.check()).count();
        // The burst, plus at most a token or two refilled while looping.
        assert!((5..=7).contains(&answered), "answered {answered} of 20");
    }

    #[test]
    fn file_budget_is_per_user_and_charges_a_minimum() {
        let l = FileByteLimiter::new(1024); // 1 MiB per minute
        assert!(l.try_consume("a", 512 * 1024));
        assert!(l.try_consume("a", 512 * 1024));
        assert!(!l.try_consume("a", 1));
        assert!(l.try_consume("b", 1024 * 1024));
        assert!(
            !l.try_consume("b", 2 * 1024 * 1024),
            "over the whole budget"
        );
        // A one-byte transfer still costs one chunk's worth.
        let l = FileByteLimiter::new(128);
        assert!(l.try_consume("a", 1));
        assert!(l.try_consume("a", 1));
        assert!(!l.try_consume("a", 1));
    }

    #[tokio::test]
    async fn loopback_is_never_limited() {
        let limiter = HandshakeRateLimiter::new(1, 1);
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for _ in 0..1000 {
            assert!(limiter.check(lo).is_ok());
        }
    }

    #[tokio::test]
    async fn burst_is_enforced_per_ip() {
        let limiter = HandshakeRateLimiter::new(60, 3);
        let a = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let b = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11));

        // Burst of 3 allowed for A
        assert!(limiter.check(a).is_ok());
        assert!(limiter.check(a).is_ok());
        assert!(limiter.check(a).is_ok());
        assert!(limiter.check(a).is_err());

        // B has its own bucket, unaffected
        assert!(limiter.check(b).is_ok());
    }
}

//! Per-tenant stream-admission rate limiting (token bucket).
//!
//! New stream admissions are metered with a token bucket keyed by the
//! authenticated client's tenant (falling back to the resolved serving
//! tenant when client authentication is disabled). Buckets refill on the
//! host monotonic clock and cap at a burst size; an empty bucket refuses the
//! stream (resetting it with a distinct error code) before any per-stream
//! region is allocated, so a stream flood costs the least possible before it
//! reaches a serving guest. Idle keys are evicted so the bucket map stays
//! bounded by active tenants rather than every tenant that ever connected.

use std::{collections::HashMap, sync::Arc, time::Duration};

use selium_guest::time::Instant;
use tokio::sync::Mutex;

/// A tenant's bucket is evicted after this long without a stream admission.
pub const ADMISSION_IDLE_EVICT_AFTER: Duration = Duration::from_secs(60);
/// Default admission burst (streams) per tenant.
pub const DEFAULT_ADMISSION_BURST: u64 = 32;
/// Default admission rate (streams/sec) per tenant. Operator configuration;
/// this is the deployable default.
pub const DEFAULT_ADMISSION_TOKENS_PER_SEC: u64 = 16;

/// Shared per-tenant stream-admission rate limiter, cloned into each
/// connection task.
#[derive(Clone)]
pub struct AdmissionLimiter {
    inner: Arc<Mutex<Inner>>,
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
    last_touch: Instant,
}

struct Inner {
    /// Token refill rate (tokens per second).
    rate: f64,
    /// Bucket capacity (the burst size).
    burst: f64,
    /// Per-tenant buckets, keyed by tenant (empty key = root/serving route).
    buckets: HashMap<String, Bucket>,
    /// Duration after which an untouched bucket is evicted.
    evict_after: Duration,
}

impl AdmissionLimiter {
    /// Builds a limiter with the supplied rate (`tokens_per_sec`) and burst.
    pub fn new(tokens_per_sec: u64, burst: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                rate: tokens_per_sec as f64,
                burst: burst as f64,
                buckets: HashMap::new(),
                evict_after: ADMISSION_IDLE_EVICT_AFTER,
            })),
        }
    }

    /// Admits one stream for `key` at host-clock time `now`, consuming a token
    /// when available. `false` means the bucket was empty: the caller must
    /// refuse the stream (the connection itself stays up).
    pub async fn allow(&self, key: &str, now: Instant) -> bool {
        self.inner.lock().await.allow(key, now)
    }

    /// Number of live buckets, for test observability.
    pub async fn bucket_count(&self) -> usize {
        self.inner.lock().await.buckets.len()
    }
}

impl Inner {
    fn allow(&mut self, key: &str, now: Instant) -> bool {
        // Opportunistically evict idle keys so the map stays bounded by
        // active tenants.
        self.evict_idle(now);

        let bucket = self
            .buckets
            .entry(key.to_string())
            .or_insert_with(|| Bucket {
                tokens: self.burst,
                last_refill: now,
                last_touch: now,
            });

        // Refill on the host clock, capped at the burst size.
        let elapsed = now.saturating_duration_since(bucket.last_refill);
        bucket.tokens = (bucket.tokens + elapsed.as_secs_f64() * self.rate).min(self.burst);
        bucket.last_refill = now;
        bucket.last_touch = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn evict_idle(&mut self, now: Instant) {
        let evict_after = self.evict_after;
        self.buckets
            .retain(|_, bucket| now.saturating_duration_since(bucket.last_touch) < evict_after);
    }
}

/// Refuses an accepted stream cheaply: resets the send direction and stops
/// the receive direction with the admission-refused error code — distinct
/// from the handshake refusal code — leaving the connection up for other
/// streams.
pub fn refuse_stream(mut send: quinn::SendStream, mut recv: quinn::RecvStream) {
    let code = crate::ADMISSION_REFUSED_ERROR_CODE.into();
    // Best-effort: the stream may already be closed by the peer.
    drop(send.reset(code));
    drop(recv.stop(code));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nanos(seconds: u64) -> Instant {
        Instant::from_nanos(seconds.saturating_mul(1_000_000_000))
    }

    /// Task 6.2: a burst up to the bucket capacity is admitted; beyond it the
    /// bucket deprives until the host clock refills it.
    #[tokio::test]
    async fn bucket_deprives_when_empty_and_refills_over_time() {
        // One token per second, burst of two.
        let limiter = AdmissionLimiter::new(1, 2);

        // The initial burst is admitted.
        assert!(limiter.allow("acme", nanos(0)).await);
        assert!(limiter.allow("acme", nanos(0)).await);
        // The third stream within the same instant is deprived.
        assert!(!limiter.allow("acme", nanos(0)).await);

        // Two seconds later two tokens have accrued: the burst is available
        // again and consumed.
        assert!(limiter.allow("acme", nanos(2)).await);
        assert!(limiter.allow("acme", nanos(2)).await);
        assert!(!limiter.allow("acme", nanos(2)).await);
    }

    /// Task 6.3: an idle key is dropped so the bucket map does not grow
    /// without bound.
    #[tokio::test]
    async fn idle_keys_are_evicted() {
        let limiter = AdmissionLimiter::new(1, 1);
        assert!(limiter.allow("acme", nanos(0)).await);
        assert_eq!(limiter.bucket_count().await, 1);

        // Long after acme's last admission, a different tenant connects: the
        // sweep drops the idle acme bucket and only beta's remains.
        assert!(limiter.allow("beta", nanos(120)).await);
        let inner = limiter.inner.lock().await;
        assert!(!inner.buckets.contains_key("acme"));
        assert_eq!(inner.buckets.len(), 1);
    }
}

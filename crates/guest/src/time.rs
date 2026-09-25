//! Time primitives for Selium guests.
//!
//! Provides the [`Instant`] type backed by the hostcall monotonic clock, and a
//! [`Timer`] for the guest's cooperative single-threaded scheduler.

use std::{
    ops::{Add, AddAssign, Sub},
    time::Duration,
};

use selium_abi::{HostcallOutput, HostcallRequest};

use crate::{
    GuestError, Result,
    hostcall::{HostcallFuture, hostcall_async, hostcall_ready},
};

/// A measurement of the monotonic clock, backed by the Selium host.
///
/// `Instant` mirrors the [`std::time::Instant`] API but uses the hostcall-based
/// `time_monotonic` clock as its source, making it reliable on `wasm32`
/// targets where [`std::time::Instant::now`] is unavailable or unreliable
/// depending on the WASM runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant {
    nanos: u64,
}

/// Async timer that provides deadline-based wakeups via the `Sleep` hostcall.
///
/// `Timer` implements [`std::future::Future`] and, when the `quinn` feature is
/// enabled, `quinn::AsyncTimer`. It is used by the Quinn transport for
/// timeout management.
pub struct Timer {
    deadline: Instant,
    sleep_future: Option<HostcallFuture>,
}

impl Instant {
    /// The smallest possible `Instant` value (epoch start of the hostcall clock).
    pub const MIN: Instant = Instant { nanos: u64::MIN };

    /// The largest possible `Instant` value.
    pub const MAX: Instant = Instant { nanos: u64::MAX };

    /// Creates an `Instant` from a raw count of nanoseconds since the
    /// hostcall monotonic epoch.
    pub const fn from_nanos(nanos: u64) -> Self {
        Self { nanos }
    }

    /// Returns the raw count of nanoseconds since the hostcall monotonic epoch.
    pub const fn as_nanos(self) -> u64 {
        self.nanos
    }

    /// Returns an `Instant` corresponding to "now" according to the
    /// hostcall monotonic clock.
    #[expect(clippy::unreachable, reason = "native targets have no WASM hostcall")]
    pub fn now() -> Result<Self> {
        match hostcall_ready(HostcallRequest::TimeMonotonic) {
            Ok(HostcallOutput::U64(nanos)) => Ok(Self { nanos }),
            Err(e) => Err(e),
            Ok(_) => unreachable!(), // only reached when consumed in non-wasm context
        }
    }

    /// Returns the amount of time elapsed from `earlier` to `self`.
    ///
    /// # Panics
    ///
    /// Panics if `earlier` is later than `self`.
    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.checked_duration_since(earlier)
            .expect("supplied instant is later than self")
    }

    /// Returns the amount of time elapsed from `earlier` to `self`, or
    /// `None` if `earlier` is later than `self`.
    pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.nanos
            .checked_sub(earlier.nanos)
            .map(Duration::from_nanos)
    }

    /// Returns the amount of time elapsed from `earlier` to `self`, or
    /// zero if `earlier` is later than `self`.
    pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
        Duration::from_nanos(self.nanos.saturating_sub(earlier.nanos))
    }

    /// Returns the amount of time elapsed since this `Instant` was created.
    pub fn elapsed(&self) -> Result<Duration> {
        Ok(Self::now()?.saturating_duration_since(*self))
    }

    /// Returns `Some(t)` where `t` is the time `self + duration`, or `None`
    /// if overflow occurred.
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.nanos
            .checked_add(duration.as_nanos() as u64)
            .map(|nanos| Self { nanos })
    }

    /// Returns `Some(t)` where `t` is the time `self - duration`, or `None`
    /// if underflow occurred.
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        self.nanos
            .checked_sub(duration.as_nanos() as u64)
            .map(|nanos| Self { nanos })
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    /// # Panics
    ///
    /// Panics if overflow occurs.
    fn add(self, rhs: Duration) -> Self {
        self.checked_add(rhs)
            .expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, rhs: Duration) {
        self.nanos = self
            .nanos
            .checked_add(rhs.as_nanos() as u64)
            .expect("overflow when adding duration to instant");
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;

    /// # Panics
    ///
    /// Panics if underflow occurs.
    fn sub(self, rhs: Duration) -> Self {
        self.checked_sub(rhs)
            .expect("underflow when subtracting duration from instant")
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    /// # Panics
    ///
    /// Panics if `other` is later than `self`.
    fn sub(self, other: Instant) -> Duration {
        self.duration_since(other)
    }
}

impl Timer {
    /// Creates a new timer that will expire at the given deadline.
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            sleep_future: None,
        }
    }

    /// Cancels any in-flight sleep operation by dropping the future.
    pub fn cancel_wait(&mut self) {
        self.sleep_future = None;
    }

    /// Updates the deadline, cancelling any in-flight sleep.
    pub fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = deadline;
        self.cancel_wait();
    }
}

impl std::fmt::Debug for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timer")
            .field("deadline", &self.deadline)
            .finish()
    }
}

impl std::future::Future for Timer {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let Ok(now) = Instant::now() else {
            return std::task::Poll::Pending;
        };
        if now >= self.deadline {
            self.sleep_future = None;
            return std::task::Poll::Ready(());
        }

        if self.sleep_future.is_none() {
            let remaining = self.deadline - now;
            let millis = remaining.as_millis() as u64;
            self.sleep_future = Some(hostcall_async(HostcallRequest::Sleep { millis }));
        }

        if let Some(ref mut fut) = self.sleep_future {
            match std::pin::Pin::new(fut).poll(cx) {
                std::task::Poll::Ready(_) => {
                    self.sleep_future = None;
                    std::task::Poll::Ready(())
                }
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        } else {
            std::task::Poll::Pending
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.cancel_wait();
    }
}

/// Returns the current wall-clock time as nanoseconds since the UNIX epoch.
///
/// This function issues a [`HostcallRequest::TimeNow`] hostcall.
pub fn now() -> Result<u64> {
    match hostcall_ready(HostcallRequest::TimeNow)? {
        HostcallOutput::U64(nanos) => Ok(nanos),
        _ => Err(GuestError::UnexpectedHostcallOutput),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_arithmetic() {
        let t0 = Instant::from_nanos(1_000_000);
        let t1 = Instant::from_nanos(2_000_000);
        assert_eq!(t1.duration_since(t0), Duration::from_micros(1000));
        assert_eq!(t0.saturating_duration_since(t1), Duration::ZERO);

        let added = t0.checked_add(Duration::from_micros(500)).unwrap();
        assert_eq!(added.as_nanos(), 1_500_000);

        let subbed = t1.checked_sub(Duration::from_micros(500)).unwrap();
        assert_eq!(subbed.as_nanos(), 1_500_000);
    }

    #[test]
    fn instant_duration_since_normal_order() {
        let t0 = Instant::from_nanos(2_000_000);
        let t1 = Instant::from_nanos(1_000_000);
        assert_eq!(t0.duration_since(t1), Duration::from_micros(1000));
    }

    #[test]
    fn instant_duration_since_reverse_panics() {
        let t0 = Instant::from_nanos(1_000_000);
        let t1 = Instant::from_nanos(2_000_000);
        let result = std::panic::catch_unwind(|| t0.duration_since(t1));
        result.unwrap_err(); // panics because t0 < t1
    }

    #[test]
    fn instant_checked_duration_since_reverse_is_none() {
        let t0 = Instant::from_nanos(1_000_000);
        let t1 = Instant::from_nanos(2_000_000);
        assert!(t0.checked_duration_since(t1).is_none());
    }

    #[test]
    fn instant_order() {
        let a = Instant::from_nanos(100);
        let b = Instant::from_nanos(200);
        assert!(a < b);
        assert!(b > a);
        assert!(a <= a);
        assert!(b >= b);
    }
}

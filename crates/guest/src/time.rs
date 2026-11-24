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
        let nanos = u64::try_from(duration.as_nanos()).ok()?;
        self.nanos.checked_add(nanos).map(|nanos| Self { nanos })
    }

    /// Returns `Some(t)` where `t` is the time `self - duration`, or `None`
    /// if underflow occurred.
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        let nanos = u64::try_from(duration.as_nanos()).ok()?;
        self.nanos.checked_sub(nanos).map(|nanos| Self { nanos })
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
        *self = self
            .checked_add(rhs)
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
    type Output = Result<()>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<()>> {
        loop {
            let now = match Instant::now() {
                Ok(now) => now,
                Err(error) => return std::task::Poll::Ready(Err(error)),
            };
            // Re-check on every poll, including after a sleep wake: a spurious
            // or truncated wake must not complete the timer early.
            if now >= self.deadline {
                self.sleep_future = None;
                return std::task::Poll::Ready(Ok(()));
            }

            let slept = match self.sleep_future.as_mut() {
                Some(future) => match std::pin::Pin::new(future).poll(cx) {
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                    std::task::Poll::Ready(result) => Some(result),
                },
                None => None,
            };

            match slept {
                Some(Ok(_)) => {
                    // The sleep returned; loop back and re-check the deadline.
                    self.sleep_future = None;
                }
                Some(Err(error)) => {
                    self.sleep_future = None;
                    return std::task::Poll::Ready(Err(error));
                }
                None => {
                    // A wake that returns before the deadline (a truncated or
                    // spurious sleep) leaves a positive remainder, so the timer
                    // asks again instead of reporting early completion.
                    let Some(millis) = remaining_sleep(now, self.deadline) else {
                        self.sleep_future = None;
                        return std::task::Poll::Ready(Ok(()));
                    };
                    self.sleep_future = Some(hostcall_async(HostcallRequest::Sleep { millis }));
                }
            }
        }
    }
}

/// The sleep a timer must still request to reach `deadline` from `now`, or
/// `None` when the deadline has been reached.
///
/// This is the at-or-after-deadline rule in one place: a wake that arrives
/// early yields a positive duration (so the timer sleeps again), never an
/// early completion.
fn remaining_sleep(now: Instant, deadline: Instant) -> Option<u64> {
    if now >= deadline {
        return None;
    }
    Some(sleep_millis(deadline.saturating_duration_since(now)))
}

/// Converts a positive remaining duration to the sleep request's millisecond
/// granularity, rounding up so the requested sleep cannot complete before the
/// deadline.
fn sleep_millis(remaining: Duration) -> u64 {
    let millis = remaining.as_nanos().div_ceil(1_000_000);
    u64::try_from(millis).unwrap_or(u64::MAX).max(1)
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
    fn durations_above_u64_nanoseconds_never_truncate() {
        let too_large = Duration::new(u64::MAX, 999_999_999);
        assert!(too_large.as_nanos() > u128::from(u64::MAX));
        let instant = Instant::from_nanos(7);
        assert_eq!(instant.checked_add(too_large), None);
        assert_eq!(instant.checked_sub(too_large), None);

        let mut assigned = instant;
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assigned += too_large;
            }))
            .is_err()
        );
        assert_eq!(assigned, instant);
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

    // scenario: channel-wake-wait/timer-completion-is-deadline-and-error-honest/sub-millisecond-remainder
    #[test]
    fn positive_sub_millisecond_remainders_round_up() {
        assert_eq!(sleep_millis(Duration::from_nanos(1)), 1);
        assert_eq!(sleep_millis(Duration::from_micros(999)), 1);
        assert_eq!(sleep_millis(Duration::from_millis(1)), 1);
        assert_eq!(sleep_millis(Duration::from_micros(1_500)), 2);
    }

    // scenario: channel-wake-wait/timer-completion-is-deadline-and-error-honest/spurious-early-wake
    #[test]
    fn a_spurious_early_wake_re_sleeps_for_the_remainder() {
        let deadline = Instant::from_nanos(2_000_000);
        assert_eq!(
            remaining_sleep(Instant::from_nanos(1_000_000), deadline),
            Some(1),
            "a wake a millisecond early still has a remainder to sleep"
        );
        assert_eq!(
            remaining_sleep(Instant::from_nanos(1_999_999), deadline),
            Some(1),
            "a sub-millisecond remainder rounds up so completion cannot be early"
        );
        assert_eq!(
            remaining_sleep(deadline, deadline),
            None,
            "reaching the deadline completes the timer"
        );
        assert_eq!(
            remaining_sleep(Instant::from_nanos(2_000_001), deadline),
            None
        );
    }

    #[test]
    fn poll_level_early_sleep_wake_re_sleeps_until_the_deadline() {
        let deadline = Instant::from_nanos(2_000_000);
        crate::platform::install_native_hostcall_script(crate::platform::NativeHostcallScript {
            time: std::collections::VecDeque::from([
                selium_abi::CompletionState::Ready(HostcallOutput::U64(0)),
                selium_abi::CompletionState::Ready(HostcallOutput::U64(0)),
                selium_abi::CompletionState::Ready(HostcallOutput::U64(0)),
                selium_abi::CompletionState::Ready(HostcallOutput::U64(0)),
                selium_abi::CompletionState::Ready(HostcallOutput::U64(0)),
                selium_abi::CompletionState::Ready(HostcallOutput::U64(deadline.as_nanos())),
            ]),
            sleep: std::collections::VecDeque::from([
                std::collections::VecDeque::from([
                    selium_abi::CompletionState::Pending { operation_id: 3 },
                    selium_abi::CompletionState::Ready(HostcallOutput::U64(0)),
                ]),
                std::collections::VecDeque::from([selium_abi::CompletionState::Pending {
                    operation_id: 8,
                }]),
            ]),
            ..crate::platform::NativeHostcallScript::default()
        });
        let mut timer = Timer::new(deadline);
        let waker = crate::async_runtime::test_task_waker();
        let mut context = std::task::Context::from_waker(&waker);
        assert!(
            std::pin::Pin::new(&mut timer)
                .poll(&mut context)
                .is_pending()
        );
        let second = std::pin::Pin::new(&mut timer).poll(&mut context);
        assert!(
            second.is_pending(),
            "an early sleep completion must re-arm for the remaining deadline: {second:?}"
        );
        assert!(matches!(
            std::pin::Pin::new(&mut timer).poll(&mut context),
            std::task::Poll::Ready(Ok(()))
        ));
        assert_eq!(crate::platform::native_hostcall_live_operations(), 0);
        crate::platform::clear_native_hostcall_script();
    }

    #[test]
    fn a_real_sleep_hostcall_failure_is_returned_from_timer_poll() {
        crate::platform::install_native_hostcall_script(crate::platform::NativeHostcallScript {
            time: std::collections::VecDeque::from([
                selium_abi::CompletionState::Ready(HostcallOutput::U64(0)),
                selium_abi::CompletionState::Ready(HostcallOutput::U64(0)),
            ]),
            sleep: std::collections::VecDeque::from([std::collections::VecDeque::from([
                selium_abi::CompletionState::Failed(selium_abi::AbiError::new(
                    selium_abi::AbiErrorCode::Internal,
                    "sleep failed",
                )),
            ])]),
            ..crate::platform::NativeHostcallScript::default()
        });
        let mut timer = Timer::new(Instant::MAX);
        let waker = crate::async_runtime::test_task_waker();
        let mut context = std::task::Context::from_waker(&waker);
        let error = match std::pin::Pin::new(&mut timer).poll(&mut context) {
            std::task::Poll::Ready(Err(error)) => error,
            other => panic!("expected a real sleep failure, got {other:?}"),
        };
        assert_eq!(
            error.abi_code(),
            Some(selium_abi::AbiErrorCode::Internal),
            "unexpected error: {error:?}"
        );
        assert_eq!(crate::platform::native_hostcall_live_operations(), 0);
        crate::platform::clear_native_hostcall_script();
    }

    // scenario: channel-wake-wait/timer-completion-is-deadline-and-error-honest/sleep-hostcall-fails
    #[test]
    fn timer_propagates_a_sleep_failure_instead_of_pending_forever() {
        // On the native test target the clock hostcall has no host, so the
        // timer must surface that failure rather than park the task.
        let mut timer = Timer::new(Instant::MAX);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match std::pin::Pin::new(&mut timer).poll(&mut context) {
            std::task::Poll::Ready(Err(_)) => {}
            other => panic!("expected a surfaced clock failure, got {other:?}"),
        }
    }
}

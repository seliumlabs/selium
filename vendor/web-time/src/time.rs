//! Clock types for `wasm32-unknown-unknown`, driven by a custom time source.
//!
//! Vendored and trimmed from [`web-time`](https://github.com/daxpedda/web-time)
//! (MIT OR Apache-2.0); see the crate root for background. Unlike upstream
//! there is no JS (`wasm-bindgen`) fallback: a source must be registered with
//! [`set_custom_time_source`], otherwise the `now()` methods panic.

use core::{
    fmt,
    ops::{Add, AddAssign, Sub, SubAssign},
};
use std::sync::OnceLock;

// Re-export the Wasm-safe parts of `std::time` (principally `Duration`), then
// shadow the Web-broken clock types with the local definitions below.
pub use std::time::*;

/// Holds the custom time source registered with [`set_custom_time_source`], if
/// any.
static CUSTOM_SOURCE: OnceLock<TimeSource> = OnceLock::new();
/// See [`std::time::UNIX_EPOCH`].
pub const UNIX_EPOCH: SystemTime = SystemTime::UNIX_EPOCH;

/// A time source that isn't the JS engine, registered with
/// [`set_custom_time_source`].
///
/// Both functions return nanoseconds:
///
/// - `monotonic_ns` is the current monotonic time since an arbitrary, fixed
///   origin. It must never go backwards.
/// - `wall_clock_ns` is the current wall-clock time since the UNIX epoch.
#[derive(Clone, Copy, Debug)]
pub struct TimeSource {
    /// Returns the current monotonic time in nanoseconds since an arbitrary,
    /// fixed origin.
    pub monotonic_ns: fn() -> u64,
    /// Returns the current wall-clock time in nanoseconds since the UNIX
    /// epoch.
    pub wall_clock_ns: fn() -> u64,
}

/// A measurement of a monotonically non-decreasing clock.
///
/// Mirrors [`std::time::Instant`] semantics but reads the registered custom
/// source (see [`set_custom_time_source`]).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Instant(Duration);

/// A wall-clock time.
///
/// Mirrors [`std::time::SystemTime`] semantics but reads the registered custom
/// source (see [`set_custom_time_source`]).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SystemTime(Duration);

/// An error returned from [`SystemTime::duration_since`].
#[derive(Clone, Debug)]
pub struct SystemTimeError(Duration);

impl Instant {
    /// Returns an `Instant` corresponding to "now".
    ///
    /// # Panics
    ///
    /// Panics if no custom time source has been registered.
    #[must_use]
    pub fn now() -> Self {
        Self(monotonic())
    }

    /// Returns the amount of time elapsed from `earlier` to `self`, or zero if
    /// `earlier` is later than `self`.
    #[must_use]
    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.checked_duration_since(earlier).unwrap_or_default()
    }

    /// Returns the amount of time elapsed from `earlier` to `self`, or `None`
    /// if `earlier` is later than `self`.
    #[must_use]
    pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    /// Returns the amount of time elapsed from `earlier` to `self`, or zero if
    /// `earlier` is later than `self`.
    #[must_use]
    pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
        self.checked_duration_since(earlier).unwrap_or_default()
    }

    /// Returns the amount of time elapsed since this `Instant` was created.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        Self::now() - *self
    }

    /// Returns `Some(t)` where `t` is the time `self + duration`, or `None`
    /// if overflow occurred.
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }

    /// Returns `Some(t)` where `t` is the time `self - duration`, or `None`
    /// if underflow occurred.
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration).map(Self)
    }
}

impl Add<Duration> for Instant {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self {
        self.checked_add(rhs)
            .expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for Instant {
    type Output = Self;

    fn sub(self, rhs: Duration) -> Self {
        self.checked_sub(rhs)
            .expect("overflow when subtracting duration from instant")
    }
}

impl Sub<Self> for Instant {
    type Output = Duration;

    /// Returns the amount of time elapsed from `rhs` to `self`, or zero if
    /// `rhs` is later than `self`.
    fn sub(self, rhs: Self) -> Duration {
        self.duration_since(rhs)
    }
}

impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl SystemTime {
    /// The UNIX epoch (1970-01-01T00:00:00Z).
    pub const UNIX_EPOCH: Self = Self(Duration::ZERO);

    /// Returns the current wall-clock time.
    ///
    /// # Panics
    ///
    /// Panics if no custom time source has been registered.
    #[must_use]
    pub fn now() -> Self {
        Self(wall_clock())
    }

    /// Returns the elapsed time since `earlier`, or an error if `earlier` is
    /// later than `self`.
    pub fn duration_since(&self, earlier: Self) -> Result<Duration, SystemTimeError> {
        self.0
            .checked_sub(earlier.0)
            .ok_or_else(|| SystemTimeError(earlier.0 - self.0))
    }

    /// Returns the elapsed time since this `SystemTime` was created.
    pub fn elapsed(&self) -> Result<Duration, SystemTimeError> {
        Self::now().duration_since(*self)
    }

    /// Returns `Some(t)` where `t` is the time `self + duration`, or `None`
    /// if overflow occurred.
    pub fn checked_add(&self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }

    /// Returns `Some(t)` where `t` is the time `self - duration`, or `None`
    /// if underflow occurred.
    pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration).map(Self)
    }
}

impl Add<Duration> for SystemTime {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self {
        self.checked_add(rhs)
            .expect("overflow when adding duration to `SystemTime`")
    }
}

impl AddAssign<Duration> for SystemTime {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for SystemTime {
    type Output = Self;

    fn sub(self, rhs: Duration) -> Self {
        self.checked_sub(rhs)
            .expect("overflow when subtracting duration from `SystemTime`")
    }
}

impl SubAssign<Duration> for SystemTime {
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl SystemTimeError {
    /// Returns the magnitude of the time difference between the two inputs.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.0
    }
}

impl fmt::Display for SystemTimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("second time provided was later than self")
    }
}

impl std::error::Error for SystemTimeError {}

/// Registers a custom time source. The first call wins; subsequent
/// registrations are ignored.
pub fn set_custom_time_source(source: TimeSource) {
    let _ = CUSTOM_SOURCE.set(source);
}

/// Returns the custom source if one was registered.
fn custom_source() -> Option<&'static TimeSource> {
    CUSTOM_SOURCE.get()
}

/// Resolves the current monotonic time.
fn monotonic() -> Duration {
    let Some(source) = custom_source() else {
        no_time_source()
    };
    Duration::from_nanos((source.monotonic_ns)())
}

/// Fails because no time source is available on this target.
#[cold]
fn no_time_source() -> ! {
    panic!(
        "web-time: no time source registered on this target; call \
         `web_time::set_custom_time_source` first"
    )
}

/// Resolves the current wall-clock time.
fn wall_clock() -> Duration {
    let Some(source) = custom_source() else {
        no_time_source()
    };
    Duration::from_nanos((source.wall_clock_ns)())
}

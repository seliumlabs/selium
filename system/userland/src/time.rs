//! Guest-side time helpers.

use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use selium_abi::TimeNow;
#[cfg(target_arch = "wasm32")]
use selium_abi::TimeSleep;

use crate::driver::DriverError;
#[cfg(target_arch = "wasm32")]
use crate::driver::{DriverFuture, RkyvDecoder, encode_args};

/// Snapshot of the host clock values.
pub use selium_abi::TimeNow as Now;

/// Fetch the current host time values.
#[cfg(target_arch = "wasm32")]
pub async fn now() -> Result<TimeNow, DriverError> {
    let args = encode_args(&())?;
    DriverFuture::<time_now::Module, RkyvDecoder<TimeNow>>::new(&args, 16, RkyvDecoder::new())?
        .await
}

/// Fetch the current time values, using the local clock when running natively.
#[cfg(not(target_arch = "wasm32"))]
pub async fn now() -> Result<TimeNow, DriverError> {
    Ok(TimeNow {
        unix_ms: unix_ms(),
        monotonic_ms: monotonic_ms(),
    })
}

/// Sleep for the provided duration.
#[cfg(target_arch = "wasm32")]
pub async fn sleep(duration: Duration) -> Result<(), DriverError> {
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let args = encode_args(&TimeSleep { duration_ms })?;
    DriverFuture::<time_sleep::Module, RkyvDecoder<()>>::new(&args, 0, RkyvDecoder::new())?.await?;
    Ok(())
}

/// Sleep for the provided duration using the local clock.
#[cfg(not(target_arch = "wasm32"))]
pub async fn sleep(duration: Duration) -> Result<(), DriverError> {
    std::thread::sleep(duration);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn monotonic_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

driver_module!(time_now, TIME_NOW, "selium::time::now");
driver_module!(time_sleep, TIME_SLEEP, "selium::time::sleep");

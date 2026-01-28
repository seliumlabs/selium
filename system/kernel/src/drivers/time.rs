//! Hostcall drivers for time access.

use std::{
    future::Future,
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use wasmtime::Caller;

use crate::{
    guest_data::GuestResult,
    operation::{Contract, Operation},
    registry::InstanceRegistry,
};
use selium_abi::{TimeNow, TimeSleep};

type TimeOps = (
    std::sync::Arc<Operation<TimeNowDriver>>,
    std::sync::Arc<Operation<TimeSleepDriver>>,
);

/// Hostcall driver that returns the current host time.
pub struct TimeNowDriver;
/// Hostcall driver that sleeps for the requested duration.
pub struct TimeSleepDriver;

impl Contract for TimeNowDriver {
    type Input = ();
    type Output = TimeNow;

    fn to_future(
        &self,
        _caller: &mut Caller<'_, InstanceRegistry>,
        _input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        std::future::ready(Ok(now()))
    }
}

impl Contract for TimeSleepDriver {
    type Input = TimeSleep;
    type Output = ();

    fn to_future(
        &self,
        _caller: &mut Caller<'_, InstanceRegistry>,
        input: Self::Input,
    ) -> impl Future<Output = GuestResult<Self::Output>> + 'static {
        let duration = Duration::from_millis(input.duration_ms);
        async move {
            tokio::time::sleep(duration).await;
            Ok(())
        }
    }
}

fn now() -> TimeNow {
    TimeNow {
        unix_ms: unix_ms(),
        monotonic_ms: monotonic_ms(),
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn monotonic_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Build hostcall operations for time access.
pub fn operations() -> TimeOps {
    (
        Operation::from_hostcall(
            TimeNowDriver,
            selium_abi::hostcall_contract!(TIME_NOW),
        ),
        Operation::from_hostcall(
            TimeSleepDriver,
            selium_abi::hostcall_contract!(TIME_SLEEP),
        ),
    )
}

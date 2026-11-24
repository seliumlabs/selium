//! Clock and sleep hostcall family.
//!
//! `TimeNow`, `TimeMonotonic`, and `Sleep` are control hostcalls: they carry no
//! data plane, but they share the guest clock offset and the timer driver's
//! deadline queue. Keeping them together makes the clock contract (guest
//! offset applied to both clocks, bounded sleep deadlines) reviewable in one
//! place.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use selium_abi::{AbiError, AbiErrorCode, HostcallOutput, ProcessId};

use super::HostOperationState;
use crate::Runtime;

impl Runtime {
    pub(super) fn hostcall_time_now(
        &self,
        _process_id: ProcessId,
    ) -> std::result::Result<HostOperationState, AbiError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos() as u64;
        let nanos = nanos.saturating_add(self.guest_clock_offset_nanos());
        Ok(HostOperationState::Ready(HostcallOutput::U64(nanos)))
    }

    pub(super) fn hostcall_time_monotonic(
        &self,
        _process_id: ProcessId,
    ) -> std::result::Result<HostOperationState, AbiError> {
        static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let nanos = EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64;
        let nanos = nanos.saturating_add(self.guest_clock_offset_nanos());
        Ok(HostOperationState::Ready(HostcallOutput::U64(nanos)))
    }

    pub(super) fn hostcall_sleep(
        &self,
        _process_id: ProcessId,
        millis: u64,
    ) -> std::result::Result<HostOperationState, AbiError> {
        if millis > selium_abi::MAX_SLEEP_MILLIS {
            return Err(AbiError::new(
                AbiErrorCode::MalformedPayload,
                format!(
                    "Sleep duration {millis}ms exceeds the supported maximum of {}ms",
                    selium_abi::MAX_SLEEP_MILLIS
                ),
            ));
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(millis))
            .ok_or_else(|| {
                AbiError::new(
                    AbiErrorCode::MalformedPayload,
                    format!("Sleep duration {millis}ms cannot be represented"),
                )
            })?;
        Ok(HostOperationState::SleepWait { deadline })
    }
}

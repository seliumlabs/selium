use std::collections::HashMap;

use parking_lot::Mutex;
use selium_abi::{MeteringObservation, ProcessId};

/// Host-side metering projector accumulator.
///
/// The projector maintains per-process **cumulative** instruction and
/// bandwidth counters and per-process **current** memory and storage gauges.
/// Instruction and memory readings are fed from the engine's per-instance meter
/// (the Wasmtiny `InstanceStats` snapshot) after every reactor poll and on each
/// tick; bandwidth is fed by the network instrumentation and storage by the
/// storage allocation paths. `Runtime::metering_tick` folds these into a
/// per-process [`MeteringObservation`] and projects it into the kernel.
#[derive(Default)]
pub(crate) struct MeteringProjector {
    /// Cumulative executed guest instructions per process (engine-fed).
    pub(crate) cpu: Mutex<HashMap<ProcessId, u64>>,
    /// Current committed linear-memory bytes per process (engine-fed).
    pub(crate) memory: Mutex<HashMap<ProcessId, u64>>,
    /// Cumulative bandwidth bytes per process.
    pub(crate) bandwidth: Mutex<HashMap<ProcessId, u64>>,
    /// Current storage bytes per process (append/put bytes).
    pub(crate) storage: Mutex<HashMap<ProcessId, u64>>,
}

impl MeteringProjector {
    /// Records the engine's cumulative executed-instruction count and committed
    /// linear-memory gauge for a process. The engine meter is authoritative and
    /// monotonic, so the latest reading replaces the previous one.
    pub(crate) fn observe_engine(
        &self,
        process_id: ProcessId,
        instructions: u64,
        memory_bytes: u64,
    ) {
        self.cpu.lock().insert(process_id, instructions);
        self.memory.lock().insert(process_id, memory_bytes);
    }

    /// Accumulates bandwidth bytes for a process (instrumentation hook).
    pub(crate) fn record_bandwidth(&self, process_id: ProcessId, bytes: u64) {
        *self.bandwidth.lock().entry(process_id).or_insert(0) += bytes;
    }

    /// Accumulates storage bytes for a process (storage allocation hook).
    pub(crate) fn record_storage(&self, process_id: ProcessId, bytes: u64) {
        *self.storage.lock().entry(process_id).or_insert(0) += bytes;
    }

    /// Removes a process's accumulation on teardown.
    pub(crate) fn remove(&self, process_id: ProcessId) {
        self.cpu.lock().remove(&process_id);
        self.memory.lock().remove(&process_id);
        self.bandwidth.lock().remove(&process_id);
        self.storage.lock().remove(&process_id);
    }

    /// Builds the projected observation for a process: cumulative instructions
    /// and bandwidth; current memory and storage gauges.
    pub(crate) fn project(&self, process_id: ProcessId) -> MeteringObservation {
        let cpu_instructions = self.cpu.lock().get(&process_id).copied().unwrap_or(0);
        let memory_bytes = self.memory.lock().get(&process_id).copied().unwrap_or(0);
        let bandwidth_bytes = self.bandwidth.lock().get(&process_id).copied().unwrap_or(0);
        let storage_bytes = self.storage.lock().get(&process_id).copied().unwrap_or(0);
        MeteringObservation {
            cpu_instructions,
            memory_bytes,
            storage_bytes,
            bandwidth_bytes,
        }
    }
}

//! Multithreaded guest execution: a dedicated pool of OS worker threads
//! running one AOT-compiled instance over the guest's shared linear memory.
//!
//! A multithreaded guest (one whose module exports `__selium_guest_worker`)
//! is AOT-compiled at load and executed concurrently: the entrypoint runs on
//! the shared instance at spawn, then `worker_count` OS threads each enter
//! the worker export, which drives the guest's work-stealing executor until
//! the process exits. Wakes are delivered by atomic notify on the guest's
//! shared wake word — the host never drives the reactor as a unit — and a
//! faulted worker stops the pool and reaps the process (see the
//! `guest-worker-pool` spec).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use parking_lot::Mutex;
use tracing::debug;
use wasmtiny::{
    RegionProt, SharedRegionId, WasmError, WasmValue,
    aot::{AotInstance, AotLoader, AotStore},
    runtime::TrapCode,
};
use wasmtiny_aotc::{CompilerConfig, compile_artifact};

use crate::{
    Error, Result,
    config::SystemGuestArg,
    error::map_wasm_error,
    mailbox::GuestMailbox,
    module_probe::WORKER_ENTRY_EXPORT,
    runtime::Runtime,
    wasm::{decode_wasm_arguments, encode_wasm_value},
};

/// How long teardown waits for a stopped worker pool to join before deciding
/// it is wedged and reclaiming the process without joining (see
/// [`MultithreadedGuest::stop_and_join`]).
const STOP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// A multithreaded guest: one AOT instance shared by a pool of worker threads.
pub(crate) struct MultithreadedGuest {
    /// The shared AOT instance all workers enter concurrently.
    instance: Arc<AotInstance>,
    /// Function index of the `__selium_guest_worker` export.
    worker_func: u32,
    /// Set once teardown has been claimed, so the worker monitor and the
    /// runtime's stop path never double-reap.
    stopped: Arc<AtomicBool>,
    /// First worker fault message, if any worker trapped.
    fault: Arc<Mutex<Option<String>>>,
    /// Set once the monitor has finished joining the workers, so the runtime's
    /// stop path never joins the monitor from inside the monitor's own reap.
    monitor_done: Arc<AtomicBool>,
    /// The monitor thread: joins the workers and reaps the process when they
    /// finish (guest exit, trap, or runtime stop).
    monitor: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Worker count this guest was provisioned with.
    pub(crate) worker_count: usize,
}

impl MultithreadedGuest {
    /// AOT-compiles `module_bytes` and instantiates the shared instance with
    /// the runtime's `selium` host-import surface.
    pub(crate) fn load(
        runtime: &Runtime,
        process_id: selium_abi::ProcessId,
        module_bytes: &[u8],
    ) -> Result<Self> {
        let artifact = compile_artifact(module_bytes, &CompilerConfig::host())
            .map_err(|error| Error::Host(format!("AOT compilation failed: {error}")))?;
        let module = AotLoader::new().load(&artifact).map_err(map_wasm_error)?;
        let store = AotStore::shared();
        let imports = runtime.runtime_aot_imports(process_id);
        let instance =
            AotInstance::instantiate(&store, &module, &imports).map_err(map_wasm_error)?;
        let worker_func = instance
            .export_func_index(WORKER_ENTRY_EXPORT)
            .ok_or_else(|| {
                Error::Host(format!(
                    "multithreaded guest lacks the {WORKER_ENTRY_EXPORT} export"
                ))
            })?;
        Ok(Self {
            instance: Arc::new(instance),
            worker_func,
            stopped: Arc::new(AtomicBool::new(false)),
            fault: Arc::new(Mutex::new(None)),
            monitor_done: Arc::new(AtomicBool::new(false)),
            monitor: Mutex::new(None),
            worker_count: 0,
        })
    }

    /// Runs the entrypoint on the shared instance (pointer arguments are
    /// staged into the guest's linear memory first).
    pub(crate) fn run_entrypoint(
        &self,
        entrypoint: &str,
        arguments: &[SystemGuestArg],
    ) -> Result<Vec<WasmValue>> {
        let args = resolve_aot_entrypoint_arguments(&self.instance, arguments)?;
        self.instance
            .invoke_export_shared(entrypoint, &args)
            .map_err(|error| {
                // Surface the trap site: the engine maps the faulting PC to its
                // wasm function index and code offset.
                match self.instance.last_trap_site() {
                    Some((func, offset, _code)) => {
                        Error::Host(format!("{error} at wasm function {func}+{offset}"))
                    }
                    None => map_wasm_error(error),
                }
            })
    }

    /// Attaches a shared region into the guest's shared linear memory,
    /// mirroring the interpreter path's `WasmApplication::attach_shared_region`
    /// (the region's pages map top-down inside the same reservation, so they
    /// coexist with the bottom-up `memory.grow` appends). Safe to call while
    /// the worker pool is executing: the AOT instance's attach takes the
    /// memory and registry locks in the same order as every other engine
    /// path. Returns the page offset the guest addresses the region at.
    pub(crate) fn attach_shared_region(
        &self,
        region_id: SharedRegionId,
        prot: RegionProt,
        reader_slot: Option<u32>,
    ) -> wasmtiny::runtime::Result<u32> {
        self.instance
            .attach_shared_region(region_id, prot, reader_slot)
    }

    /// Detaches a shared region from the guest's shared linear memory.
    pub(crate) fn detach_shared_region(
        &self,
        region_id: SharedRegionId,
    ) -> wasmtiny::runtime::Result<()> {
        self.instance.detach_shared_region(region_id)
    }

    /// Returns the shared instance's engine metering snapshot: the executed
    /// instruction count and committed owned-memory pages. Because every worker
    /// enters the same instance, a single snapshot aggregates the whole pool
    /// into one per-process reading.
    pub(crate) fn stats(&self) -> wasmtiny::runtime::Result<wasmtiny::runtime::InstanceStats> {
        self.instance.stats()
    }

    /// Sets or resets the shared instance's execution budget (maximum metering
    /// units); `None` means unbounded. Because every worker charges the shared
    /// instance's meter, the budget caps the whole pool's per-window execution.
    pub(crate) fn set_execution_budget(
        &self,
        budget: Option<u64>,
    ) -> wasmtiny::runtime::Result<()> {
        self.instance.set_execution_budget(budget)
    }

    /// Provisions `count` dedicated OS worker threads, each entering the
    /// worker export over the shared instance, plus a monitor that reaps the
    /// process when the pool finishes.
    pub(crate) fn start_workers(
        &mut self,
        runtime: Runtime,
        process_id: selium_abi::ProcessId,
        count: usize,
    ) -> Result<()> {
        self.worker_count = count;
        let worker_func = self.worker_func;
        let mut workers = Vec::with_capacity(count);
        for worker_id in 0..count as u32 {
            let instance = Arc::clone(&self.instance);
            let fault = Arc::clone(&self.fault);
            let worker_runtime = runtime.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("selium-guest-worker-{process_id}-{worker_id}"))
                .spawn(move || {
                    let result =
                        instance.invoke_shared(worker_func, &[WasmValue::I32(worker_id as i32)]);
                    if let Err(error) = result {
                        // A faulted worker: stop the remaining workers via
                        // the ABI stop word + notify, and record the fault
                        // for the monitor's reap. Each notify wakes exactly
                        // one parked worker, so deliver one per worker (the
                        // `set_stop` bump makes a worker that has not parked
                        // yet return from its wait immediately, so no
                        // delivery is lost).
                        if let Some(mailbox) = worker_runtime.mailbox(process_id) {
                            for _ in 0..count {
                                signal_stop_once(&mailbox, process_id);
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                        }
                        // Record where the trap fired: the engine maps the
                        // faulting PC to its wasm function index and code
                        // offset, which is the only in-guest location
                        // information available once the worker has faulted.
                        // Budget exhaustion is a distinct first-class outcome:
                        // the engine reports it and the runtime owns the reap,
                        // so surface it rather than a raw trap site.
                        let detail = match &error {
                            WasmError::Trap(TrapCode::ExecutionBudgetExceeded) => {
                                "exhausted its CPU instruction budget".to_string()
                            }
                            _ => match instance.last_trap_site() {
                                Some((func, offset, _code)) => {
                                    format!("{error} at wasm function {func}+{offset}")
                                }
                                None => error.to_string(),
                            },
                        };
                        let mut slot = fault.lock();
                        if slot.is_none() {
                            *slot = Some(detail);
                        }
                    }
                })
                .map_err(|error| Error::Host(format!("worker thread spawn failed: {error}")));
            match spawned {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    // Partial spawn: signal the workers already started so they
                    // observe the stop and return rather than parking forever on
                    // an instance whose pool was never completed (their stop
                    // path would otherwise never fire, since the runtime's stop
                    // only reaches a fully-registered pool). Then fail the spawn.
                    signal_stop(&runtime, process_id);
                    return Err(Error::Host(format!("worker thread spawn failed: {error}")));
                }
            }
        }
        let monitor_runtime = runtime.clone();
        let stopped = Arc::clone(&self.stopped);
        let fault = Arc::clone(&self.fault);
        let monitor_done = Arc::clone(&self.monitor_done);
        let monitor = std::thread::Builder::new()
            .name(format!("selium-guest-monitor-{process_id}"))
            .spawn(move || {
                for worker in workers {
                    if let Err(panic) = worker.join() {
                        // A worker's closure folds every trap into `fault`; a
                        // `join` error means the thread itself panicked before
                        // doing so, so record it to keep the reap classified as
                        // a fault rather than a clean exit.
                        let detail = panic
                            .downcast_ref::<&str>()
                            .map(|message| (*message).to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "worker thread panicked".to_string());
                        let mut slot = fault.lock();
                        if slot.is_none() {
                            *slot = Some(detail);
                        }
                    }
                }
                // Announce completion BEFORE the reap so the monitor's own
                // cleanup never joins the monitor thread from itself.
                monitor_done.store(true, Ordering::SeqCst);
                monitor_finished(&monitor_runtime, process_id, stopped, fault);
            })
            .map_err(|error| Error::Host(format!("worker monitor spawn failed: {error}")));
        let monitor = match monitor {
            Ok(monitor) => monitor,
            Err(error) => {
                // The monitor could not start, so nothing will ever join the
                // workers: signal the stop so they return instead of parking
                // forever, then fail the spawn.
                signal_stop(&runtime, process_id);
                return Err(error);
            }
        };
        *self.monitor.lock() = Some(monitor);
        Ok(())
    }

    /// Signals the workers to return (ABI stop word + wake-word notify) and
    /// reaps the pool. Idempotent with the monitor's own reap: whoever claims
    /// `stopped` first handles teardown, the other skips it.
    ///
    /// The stop signal is re-delivered on a short cadence until the monitor
    /// reports every worker joined: each notify releases exactly one parked
    /// worker, so an N-worker pool needs N deliveries, or the still-parked
    /// workers would sleep through the stop. The `set_stop` wake-word bump
    /// covers a worker that has not parked yet (its `wait32` returns
    /// immediately on the value mismatch), and the parked-worker timeout is a
    /// liveness backstop.
    ///
    /// If the pool does not join within [`STOP_BUDGET`], a worker is wedged —
    /// it trapped inside a critical section (leaving a lock held), or is
    /// blocked on a lock a faulted worker held, so it can never observe the
    /// stop. Joining would block teardown forever, so the wedge is recorded
    /// and the monitor is detached without joining: the caller's teardown
    /// reclaims the process anyway (Per-Worker Fault Isolation). The wedged
    /// threads hold an `Arc` to the shared instance (and its linear memory),
    /// so they leak bounded resources and cannot corrupt the reclaimed
    /// process.
    pub(crate) fn stop_and_join(&self, runtime: &Runtime, process_id: selium_abi::ProcessId) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            // Claimed teardown: signal the workers, then reap below.
            signal_stop(runtime, process_id);
        }
        if let Some(monitor) = self.monitor.lock().take() {
            if !self.monitor_done.load(Ordering::SeqCst) {
                // Wake the pool one worker per delivery until the monitor has
                // joined every worker (bounded by the stop budget).
                let deadline = std::time::Instant::now() + STOP_BUDGET;
                while !self.monitor_done.load(Ordering::SeqCst)
                    && std::time::Instant::now() < deadline
                {
                    signal_stop(runtime, process_id);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                if !self.monitor_done.load(Ordering::SeqCst) {
                    // Wedged pool: record it and detach rather than join. The
                    // caller (stop_process / cleanup_failed_process) reclaims
                    // the process; the monitor's own reap is skipped because
                    // `stopped` is already claimed.
                    runtime
                        .kernel
                        .processes()
                        .record_activity(selium_abi::ActivityEvent {
                            kind: selium_abi::ActivityKind::ProcessExited,
                            process_id: Some(process_id),
                            message: format!(
                                "multithreaded guest {process_id} worker pool wedged on stop; \
                                 reclaimed without joining the remaining workers"
                            ),
                        });
                }
            }
            // Drop the handle either way — never join: the monitor either
            // finished its join loop (it is inside `monitor_finished`) or is
            // wedged. Joining a wedged monitor would hang teardown.
            drop(monitor);
        }
    }
}

/// The monitor's reap: classify whether the pool finished by fault or normal
/// exit and tear the process down (idempotent with the runtime's stop path).
fn monitor_finished(
    runtime: &Runtime,
    process_id: selium_abi::ProcessId,
    stopped: Arc<AtomicBool>,
    fault: Arc<Mutex<Option<String>>>,
) {
    if stopped.swap(true, Ordering::SeqCst) {
        // The runtime's stop path already claimed teardown and will reap.
        return;
    }
    let guest_logs = runtime.drain_guest_log_messages(process_id);
    let fault = fault.lock().clone();
    let message = match &fault {
        Some(fault) => format!(
            "multithreaded guest {process_id} worker trapped ({fault}); recent guest logs: {guest_logs:?}"
        ),
        None => format!(
            "multithreaded guest {process_id} workers exited; recent guest logs: {guest_logs:?}"
        ),
    };
    runtime
        .kernel
        .processes()
        .record_activity(selium_abi::ActivityEvent {
            kind: selium_abi::ActivityKind::ProcessExited,
            process_id: Some(process_id),
            message,
        });
    drop(runtime.cleanup_failed_process(process_id));
}

/// Resolves structured [`SystemGuestArg`]s into the flattened `WasmValue`
/// slot list for an AOT instance. Pointer arguments have their payload copied
/// into the guest's linear memory first and become `(address, length)` slots.
fn resolve_aot_entrypoint_arguments(
    instance: &AotInstance,
    arguments: &[SystemGuestArg],
) -> Result<Vec<WasmValue>> {
    let mut encoded: Vec<Vec<u8>> = Vec::new();
    for argument in arguments {
        match argument {
            SystemGuestArg::Integer(value) => {
                encoded.push(encode_wasm_value(WasmValue::I64(*value as i64)));
            }
            SystemGuestArg::Pointer(payload) => {
                let address = write_aot_entrypoint_bytes(instance, payload)?;
                encoded.push(encode_wasm_value(WasmValue::I64(address as i64)));
                encoded.push(encode_wasm_value(WasmValue::I64(payload.len() as i64)));
            }
        }
    }
    decode_wasm_arguments(&encoded)
}

/// Signals a guest's worker pool to return (ABI stop word + wake-word bump)
/// without joining. Used when a pool cannot be fully provisioned: the workers
/// already started observe the stop and exit instead of parking forever on an
/// instance whose monitor will never join them.
fn signal_stop(runtime: &Runtime, process_id: selium_abi::ProcessId) {
    if let Some(mailbox) = runtime.mailbox(process_id) {
        signal_stop_once(&mailbox, process_id);
    }
}

/// Delivers one stop signal to `mailbox`: sets the ABI stop word and notifies
/// a parked worker. Mailbox errors are logged rather than discarded — wake
/// delivery is best-effort, but a failure during isolation or teardown must
/// not pass unnoticed.
fn signal_stop_once(mailbox: &GuestMailbox, process_id: selium_abi::ProcessId) {
    // `set_stop` also bumps the shared wake word, so a parked worker's
    // `wait32` returns on the value mismatch and it re-checks the stop word;
    // the notify covers a worker parked at the moment of the bump.
    if let Err(error) = mailbox.set_stop() {
        debug!(process_id, error = %error, "failed to set guest stop word");
    }
    if let Err(error) = mailbox.notify_wake_word(1) {
        debug!(process_id, error = %error, "failed to notify guest wake word");
    }
}

/// Grows the guest's linear memory and copies `bytes` into it, returning the
/// byte address the entrypoint should be handed as the pointer argument.
fn write_aot_entrypoint_bytes(instance: &AotInstance, bytes: &[u8]) -> Result<u64> {
    const WASM_PAGE_SIZE: u64 = 65536;
    let memory = instance.memory_handle(0).ok_or_else(|| {
        Error::Host("multithreaded guest does not expose a linear memory".to_string())
    })?;
    let mut memory = memory
        .lock()
        .map_err(|_lock_err| Error::Host("guest memory lock poisoned".to_string()))?;
    let old_bytes = u64::from(memory.size()) * WASM_PAGE_SIZE;
    let delta_pages = bytes.len().div_ceil(WASM_PAGE_SIZE as usize) as u32;
    if delta_pages > 0 {
        memory.grow(delta_pages).map_err(map_wasm_error)?;
    }
    if !bytes.is_empty() {
        memory
            .write(old_bytes as u32, bytes)
            .map_err(map_wasm_error)?;
    }
    Ok(old_bytes)
}

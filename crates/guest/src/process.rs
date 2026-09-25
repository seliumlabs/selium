use selium_abi::{
    ActivityEvent, CapabilityGrant, HostcallOutput, HostcallRequest, MeteringObservation,
    ProcessDescriptor,
};

use crate::{GuestError, Result, hostcall::hostcall_ready};

/// Guest handle for a process started through the host.
#[derive(Clone, Debug)]
pub struct Process {
    descriptor: ProcessDescriptor,
}

/// Accessor for host activity log entries.
#[derive(Clone, Copy, Debug)]
pub struct ActivityLog;

/// Accessor for process metering observations.
#[derive(Clone, Copy, Debug)]
pub struct Metering;

impl Process {
    /// Starts a process from a module, entrypoint, arguments, and grants.
    /// The child inherits the parent's tenant.
    pub fn start(
        module_id: impl Into<String>,
        entrypoint: impl Into<String>,
        arguments: Vec<Vec<u8>>,
        grants: Vec<CapabilityGrant>,
    ) -> Result<Self> {
        Self::start_for_tenant(module_id, entrypoint, arguments, grants, None)
    }

    /// Starts a process under an explicit `tenant`.
    ///
    /// `None` inherits the parent's tenant; a tenant differing from the
    /// parent's own requires an in-scope `DelegateGrants` grant — including
    /// for a root parent, so cross-tenant authority is always a grant, not
    /// the parent's bootstrap tenant (the runtime denies the spawn otherwise).
    pub fn start_for_tenant(
        module_id: impl Into<String>,
        entrypoint: impl Into<String>,
        arguments: Vec<Vec<u8>>,
        grants: Vec<CapabilityGrant>,
        tenant: Option<&str>,
    ) -> Result<Self> {
        match hostcall_ready(HostcallRequest::ProcessStart {
            module_id: module_id.into(),
            entrypoint: entrypoint.into(),
            arguments,
            grants,
            tenant: tenant.map(str::to_string),
        })? {
            HostcallOutput::Process(descriptor) => Ok(Self { descriptor }),
            _ => Err(GuestError::UnexpectedHostcallOutput),
        }
    }

    /// Returns the process descriptor.
    pub fn descriptor(&self) -> &ProcessDescriptor {
        &self.descriptor
    }

    /// Stops the process.
    pub fn stop(self) -> Result<()> {
        match hostcall_ready(HostcallRequest::ProcessStop {
            process_id: self.descriptor.local_id,
        })? {
            HostcallOutput::Empty => Ok(()),
            _ => Err(GuestError::UnexpectedHostcallOutput),
        }
    }
}

impl ActivityLog {
    /// Reads activity events starting at the cursor offset.
    pub fn read_from(cursor: usize) -> Result<Vec<ActivityEvent>> {
        match hostcall_ready(HostcallRequest::ActivityRead { cursor })? {
            HostcallOutput::ActivityEvents(events) => Ok(events),
            _ => Err(GuestError::UnexpectedHostcallOutput),
        }
    }
}

impl Metering {
    /// Reads the latest metering observation for a process, if available.
    pub fn read(process_id: u64) -> Result<Option<MeteringObservation>> {
        match hostcall_ready(HostcallRequest::MeteringRead { process_id })? {
            HostcallOutput::Metering(observation) => Ok(Some(observation)),
            HostcallOutput::Empty => Ok(None),
            _ => Err(GuestError::UnexpectedHostcallOutput),
        }
    }
}

//! Region provider backed by guest hostcalls.

use selium_abi::{HostcallOutput, HostcallRequest, RegionAllocation, RegionProt, ResourceKind};
use selium_memory::{MemoryError, Region, RegionProvider};

use crate::{error::guest_error_to_memory_error, hostcall::hostcall_ready};

/// Shared-memory region provider that delegates to the host via hostcalls.
#[derive(Default, Debug)]
pub struct HostcallRegionProvider;

impl HostcallRegionProvider {
    /// Creates a new provider.
    pub fn new() -> Self {
        Self
    }
}

impl RegionProvider for HostcallRegionProvider {
    fn allocate_for_tenant(
        &self,
        pages: u32,
        prot: RegionProt,
        purpose: ResourceKind,
        serving_tenant: Option<&str>,
    ) -> Result<Region, MemoryError> {
        // AllocRegion only reserves the region in the host; it does not map it
        // into this guest's linear memory. Attach immediately so the returned
        // `Region` is usable.
        let allocation = match hostcall_ready(HostcallRequest::AllocRegion {
            pages,
            prot,
            purpose,
            serving_tenant: serving_tenant.map(str::to_string),
        })
        .map_err(guest_error_to_memory_error)?
        {
            HostcallOutput::RegionAlloc(allocation) => allocation,
            _ => {
                return Err(MemoryError::Other(
                    "unexpected hostcall output for AllocRegion".to_string(),
                ));
            }
        };

        self.attach(allocation.region_id, None, prot)
    }

    fn attach(
        &self,
        region_id: u64,
        reader_slot: Option<u32>,
        prot: RegionProt,
    ) -> Result<Region, MemoryError> {
        match hostcall_ready(HostcallRequest::AttachRegion {
            region_id,
            reader_slot,
            prot,
        })
        .map_err(guest_error_to_memory_error)?
        {
            HostcallOutput::RegionAttach(attachment) => Ok(Region::new(
                RegionAllocation {
                    region_id,
                    page_offset: attachment.page_offset,
                },
                u64::from(attachment.len),
                None,
            )),
            _ => Err(MemoryError::Other(
                "unexpected hostcall output for AttachRegion".to_string(),
            )),
        }
    }

    fn free(&self, region_id: u64) -> Result<(), MemoryError> {
        match hostcall_ready(HostcallRequest::FreeRegion { region_id })
            .map_err(guest_error_to_memory_error)?
        {
            HostcallOutput::Empty => Ok(()),
            _ => Err(MemoryError::Other(
                "unexpected hostcall output for FreeRegion".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_preserves_the_structured_abi_error() {
        crate::platform::install_native_hostcall_script(crate::platform::NativeHostcallScript {
            immediate_failure: Some(selium_abi::AbiError::new(
                selium_abi::AbiErrorCode::QuotaExceeded,
                "quota detail",
            )),
            ..crate::platform::NativeHostcallScript::default()
        });
        let error = match HostcallRegionProvider::new().allocate(
            1,
            RegionProt::ReadWrite,
            ResourceKind::SharedMemory,
        ) {
            Err(error) => error,
            Ok(_) => panic!("allocation must fail"),
        };
        assert!(matches!(
            error,
            MemoryError::Abi(selium_abi::AbiError {
                code: selium_abi::AbiErrorCode::QuotaExceeded,
                ref message,
            }) if message == "quota detail"
        ));
        crate::platform::clear_native_hostcall_script();
    }
}

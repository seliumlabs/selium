//! Two-ring multi-memory region pair used by byte-stream channels.
//!
//! A pair is a single parent shared region carrying a two-entry
//! [`MultiMemoryHeader`] whose sub-memories are two ring channels (the same
//! layout `TcpStream` and the QUIC byte channels consume):
//!
//! - channel `0` is the "inbound" ring of the attaching peer (the owner
//!   writes to it, the attaching peer reads from it);
//! - channel `1` is the "outbound" ring (the attaching peer writes to it).
//!
//! [`create`] allocates and initialises the pair, returning both channels and
//! the parent `shared_id`; [`attach`] attaches to an existing pair and returns
//! its two channels. The RPC layer and the QUIC connector's per-stream byte
//! channels share this one layout and implementation.

use selium_abi::{RegionProt, ResourceKind};
use selium_memory::{
    HEADER_SIZE_TWO_ENTRIES, MultiMemoryHeader, RING_HEADER_SIZE, Region, WASM_PAGE_SIZE,
};
use selium_wire::error::Result;

use crate::{Channel, ChannelBackpressure, ChannelRegion, ring_buf::RingBuf};

/// Attaches to an existing two-ring region pair by parent `shared_id`.
pub fn attach(shared_id: u64) -> Result<(Channel, Channel)> {
    let region = selium_memory::region_provider()?
        .attach(shared_id, None, RegionProt::ReadWrite)
        .map_err(selium_wire::error::Error::from)?;
    let parent_mapping = region.mapping();

    let header = MultiMemoryHeader::parse(parent_mapping.backend(), 0)
        .map_err(selium_wire::error::Error::from)?;
    if header.count < 2 {
        return Err(selium_wire::error::Error::InvalidLayout);
    }

    let entry_0 = header.entry(0)?;
    let entry_1 = header.entry(1)?;

    let capacity_0 = entry_0
        .length
        .checked_sub(RING_HEADER_SIZE)
        .ok_or(selium_wire::error::Error::InvalidLayout)?;
    let capacity_1 = entry_1
        .length
        .checked_sub(RING_HEADER_SIZE)
        .ok_or(selium_wire::error::Error::InvalidLayout)?;

    let channel_0 = channel_from_sub_mapping(
        &parent_mapping,
        entry_0.offset,
        entry_0.length,
        capacity_0,
        shared_id,
    )?;
    let channel_1 = channel_from_sub_mapping(
        &parent_mapping,
        entry_1.offset,
        entry_1.length,
        capacity_1,
        shared_id,
    )?;

    Ok((channel_0, channel_1))
}

/// Creates a fresh two-ring region pair.
///
/// Returns the two initialised channels, the parent region's `shared_id`,
/// and the parent region handle (the caller's side is already attached by
/// the allocation; callers must NOT attach the region again — the runtime's
/// region provider rejects a second attach, so the mirror peer attaches via
/// [`attach`] instead). The caller owns the parent region and is
/// responsible for delivering `shared_id` to its peer (and eventually
/// freeing the region).
pub fn create(capacity_0: u64, capacity_1: u64) -> Result<(Channel, Channel, u64, Region)> {
    create_for_tenant(capacity_0, capacity_1, None)
}

/// Creates a fresh two-ring region pair minted under `serving_tenant` (see
/// [`RegionProvider::allocate_for_tenant`](selium_memory::RegionProvider)).
pub fn create_for_tenant(
    capacity_0: u64,
    capacity_1: u64,
    serving_tenant: Option<&str>,
) -> Result<(Channel, Channel, u64, Region)> {
    let len_0 = RING_HEADER_SIZE + capacity_0;
    let len_1 = RING_HEADER_SIZE + capacity_1;

    let sub_memory_0_offset = align_up(HEADER_SIZE_TWO_ENTRIES, 8);
    let sub_memory_1_offset = align_up(sub_memory_0_offset + len_0, 8);
    let total_capacity = align_up(sub_memory_1_offset + len_1, WASM_PAGE_SIZE);

    let pages = pages_for_bytes(total_capacity);
    let region = selium_memory::region_provider()?
        .allocate_for_tenant(
            pages,
            RegionProt::ReadWrite,
            ResourceKind::SharedMemory,
            serving_tenant,
        )
        .map_err(selium_wire::error::Error::from)?;
    let shared_id = region.region_id();
    let parent_mapping = region.mapping();

    MultiMemoryHeader::write_two_entries(
        parent_mapping.backend(),
        0,
        total_capacity,
        [(sub_memory_0_offset, len_0), (sub_memory_1_offset, len_1)],
    )
    .map_err(selium_wire::error::Error::from)?;

    let channel_0 = initialise_sub_channel(
        &parent_mapping,
        sub_memory_0_offset,
        len_0,
        capacity_0,
        shared_id,
    )?;
    let channel_1 = initialise_sub_channel(
        &parent_mapping,
        sub_memory_1_offset,
        len_1,
        capacity_1,
        shared_id,
    )?;

    Ok((channel_0, channel_1, shared_id, region))
}

/// Aligns a value up to the given alignment.
fn align_up(value: u64, alignment: u64) -> u64 {
    let rem = value % alignment;
    if rem == 0 {
        value
    } else {
        value + alignment - rem
    }
}

/// Wraps an existing sub-mapping as a channel without initialising it.
fn channel_from_sub_mapping(
    parent_mapping: &selium_memory::RegionMapping,
    offset: u64,
    len: u64,
    capacity: u64,
    shared_id: u64,
) -> Result<Channel> {
    let mapping = parent_mapping.sub_region(offset, len)?;
    let region = ChannelRegion::from_mapping_with_id(mapping, capacity, shared_id);
    let ring = RingBuf::wrap_region(region)?;
    Ok(Channel::from_ring(ring, ChannelBackpressure::Park))
}

/// Initialises a sub-region for a freshly-allocated channel.
fn initialise_sub_channel(
    parent_mapping: &selium_memory::RegionMapping,
    offset: u64,
    len: u64,
    capacity: u64,
    shared_id: u64,
) -> Result<Channel> {
    let mapping = parent_mapping.sub_region(offset, len)?;
    let region = ChannelRegion::from_mapping_with_id(mapping, capacity, shared_id);
    region.initialise()?;
    region.store_backpressure(ChannelBackpressure::Park.to_u8())?;
    region.store_shared_capacity(capacity)?;
    let ring = RingBuf::wrap_region(region)?;
    Ok(Channel::from_ring(ring, ChannelBackpressure::Park))
}

/// Computes the number of WASM pages needed to hold `bytes`.
fn pages_for_bytes(bytes: u64) -> u32 {
    bytes.div_ceil(WASM_PAGE_SIZE) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        crate::install_heap_provider();
    }

    #[test]
    fn create_and_attach_round_trip() {
        setup();
        let (ch0, ch1, shared_id, _region) = create(4096, 4096).expect("create");
        assert!(shared_id > 0, "parent shared_id must be non-zero");

        let (attached_0, attached_1) = attach(shared_id).expect("attach");
        assert_eq!(attached_0.ring().capacity(), ch0.ring().capacity());
        assert_eq!(attached_1.ring().capacity(), ch1.ring().capacity());

        drop(crate::free_region(shared_id));
    }
}

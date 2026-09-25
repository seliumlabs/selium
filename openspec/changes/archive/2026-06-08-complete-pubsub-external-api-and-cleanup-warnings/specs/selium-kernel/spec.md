## REMOVED Requirements

### Requirement: SharedMappingState Tracks Protection and Reader Slot Fields
**Reason**: `page_offset`, `prot`, and `reader_slot` fields in `SharedMappingState` are stored on creation but never read. Protection enforcement is handled by wasmtiny's `map_shared_region` via `mprotect` at attach time (see `per-page-memory-protection` spec), and the kernel has no post-attach need to re-inspect these values.
**Migration**: The fields are removed from `SharedMappingState`. Construction sites (`TcpListenerState`, `HostQueueState`, etc.) that set these fields no longer populate them. The remaining fields (`region_id`, `shared_id`) are sufficient for lookup, cleanup, and lifecycle management.

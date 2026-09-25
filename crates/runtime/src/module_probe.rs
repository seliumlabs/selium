//! Guest module probe for the shared-page fast path.
//!
//! Fast-path eligibility is detected from the guest's own module bytes at
//! spawn time — the ground truth the wasmtiny validator itself uses — rather
//! than inferred indirectly at attach time. Two signals are extracted:
//!
//! 1. **Shared memory declaration**: any memory entry with the shared flag
//!    set. The validator requires this for atomic instructions, so a guest
//!    built with `nightly-wasm-atomics` always declares it (the CI guest
//!    build passes `--shared-memory`/`--max-memory` link flags for exactly
//!    this reason).
//! 2. **Atomic notify opcode**: the `memory.atomic.notify` opcode sequence
//!    (`0xFE 0x00`) in the code section. A shared-memory declaration alone
//!    does not prove the guest *emits* notifies on its write path; a guest
//!    hand-linked with shared memory but built without the atomics feature
//!    would otherwise be misclassified as fast-path, its transition kicks
//!    suppressed, and its drainers left to the bounded backstop.
//!
//! The scan is heuristic in one direction only: `0xFE 0x00` can appear as
//! immediate bytes of unrelated instructions (false positive → the guest is
//! treated as fast-path-capable when it is not, degrading to the same
//! backstop-latency case), but a genuine `memory.atomic.notify` always
//! produces the sequence (never a false negative). Combined with the
//! shared-memory requirement the misclassification window is a module that
//! declares shared memory, contains a coincidental `0xFE 0x00` immediate,
//! and never actually notifies — contrived by construction.
//!
//! Detection, not configuration: there is no user-facing knob; see the
//! `shared-page-fastpath` capability spec.

const CODE_SECTION: u8 = 10;
const EXPORT_SECTION: u8 = 7;
const MEMORY_SECTION: u8 = 5;
/// The worker entry export name a multithreaded guest module must carry.
/// Presence selects multithreaded execution (dedicated worker pool); absence
/// falls back to the single-worker cooperative reactor.
pub const WORKER_ENTRY_EXPORT: &str = "__selium_guest_worker";

/// Probe result: does this module participate in the shared-page fast path?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleProbe {
    /// The module declares at least one shared memory.
    pub shared_memory: bool,
    /// The module's code section contains the `memory.atomic.notify` opcode
    /// sequence (`0xFE 0x00`).
    pub atomic_notify: bool,
    /// The module exports the multithreaded worker entry
    /// (`__selium_guest_worker`).
    pub worker_entry: bool,
}

/// Iterates the sections of a wasm module. Returns `None` when the magic
/// header or version is missing.
struct SectionCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl ModuleProbe {
    /// True when a guest built from this module is fast-path capable: it
    /// declares shared memory (validator requirement for atomics) and its
    /// code contains atomic notify opcodes (the write path emits them).
    pub fn fast_path_capable(&self) -> bool {
        self.shared_memory && self.atomic_notify
    }

    /// True when the module declares the multithreaded worker entry export,
    /// so the runtime provisions a dedicated worker pool for it. Absence
    /// selects the cooperative single-worker reactor.
    pub fn multithreaded(&self) -> bool {
        self.worker_entry
    }
}

impl<'a> SectionCursor<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() < 8
            || bytes.get(..4) != Some(b"\0asm")
            || bytes.get(4..8) != Some(&[0x01, 0x00, 0x00, 0x00])
        {
            return None;
        }
        Some(Self { bytes, pos: 8 })
    }

    /// Returns the next `(section id, payload)`, or `None` at end of module
    /// or on a malformed section header (which ends the scan).
    fn next_section(&mut self) -> Option<(u8, &'a [u8])> {
        if self.pos >= self.bytes.len() {
            return None;
        }
        let id = *self.bytes.get(self.pos)?;
        self.pos += 1;
        let size = self.read_leb_u32()?;
        let payload = self.bytes.get(self.pos..self.pos + size as usize)?;
        self.pos += size as usize;
        Some((id, payload))
    }

    fn read_leb_u32(&mut self) -> Option<u32> {
        let mut result: u32 = 0;
        let mut shift = 0;
        loop {
            let byte = *self.bytes.get(self.pos)?;
            self.pos += 1;
            result |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift >= 32 {
                return None;
            }
        }
    }
}

/// Probes a wasm module's memory section and code section for the fast-path
/// signals. Malformed input yields an all-false result — the safe fallback
/// (portable kick path) — rather than an error: a module that cannot be
/// parsed here would not have loaded far enough to attach a region anyway.
pub fn probe(module: &[u8]) -> ModuleProbe {
    let mut probe = ModuleProbe {
        shared_memory: false,
        atomic_notify: false,
        worker_entry: false,
    };

    let Some(mut cursor) = SectionCursor::new(module) else {
        return probe;
    };

    while let Some((id, payload)) = cursor.next_section() {
        match id {
            MEMORY_SECTION => probe.shared_memory |= scan_memory_section(payload),
            CODE_SECTION => probe.atomic_notify |= scan_code_section(payload),
            EXPORT_SECTION => probe.worker_entry |= scan_export_section(payload),
            _ => {}
        }
    }
    probe
}

/// True when the code section payload contains the `memory.atomic.notify`
/// opcode sequence (`0xFE 0x00`). See the module docs for the false-positive
/// analysis; there are no false negatives.
fn scan_code_section(payload: &[u8]) -> bool {
    payload.windows(2).any(|pair| pair == [0xFE, 0x00])
}

/// True when an export section payload declares the multithreaded worker
/// entry export. Returns false on malformed input (the safe cooperative
/// fallback).
fn scan_export_section(payload: &[u8]) -> bool {
    let mut cursor = SectionCursor {
        bytes: payload,
        pos: 0,
    };
    let Some(count) = cursor.read_leb_u32() else {
        return false;
    };
    for _ in 0..count {
        // Export name: LEB length + bytes.
        let Some(name_len) = cursor.read_leb_u32() else {
            return false;
        };
        let Some(name_start) = cursor.pos.checked_add(name_len as usize) else {
            return false;
        };
        let Some(name) = cursor.bytes.get(cursor.pos..name_start) else {
            return false;
        };
        if name == WORKER_ENTRY_EXPORT.as_bytes() {
            return true;
        }
        cursor.pos = name_start;
        // Export kind (1 byte) + index (LEB128).
        cursor.pos += 1;
        if cursor.read_leb_u32().is_none() {
            return false;
        }
    }
    false
}

/// True when any memory entry in a memory section payload sets the shared
/// flag (bit 1 of the limits flags byte). Stops at the first shared entry;
/// returns false on malformed input.
fn scan_memory_section(payload: &[u8]) -> bool {
    let mut cursor = SectionCursor {
        bytes: payload,
        pos: 0,
    };
    let Some(count) = cursor.read_leb_u32() else {
        return false;
    };
    for _ in 0..count {
        let Some(flags) = cursor.bytes.get(cursor.pos).copied() else {
            return false;
        };
        let shared = flags & 0x02 != 0;
        cursor.pos += 1;
        // min (and max when the has-max bit is set); both LEB128.
        if cursor.read_leb_u32().is_none() {
            return false;
        }
        if flags & 0x01 != 0 && cursor.read_leb_u32().is_none() {
            return false;
        }
        if shared {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal module header + one section.
    fn module(section_id: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = b"\0asm\x01\x00\x00\x00".to_vec();
        bytes.push(section_id);
        // Section size as LEB128 (single byte suffices for test payloads).
        bytes.push(payload.len() as u8);
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn rejects_non_module_bytes() {
        let probe = probe(b"not a module");
        assert!(!probe.fast_path_capable());
    }

    #[test]
    fn non_shared_module_without_notify_is_not_capable() {
        // Memory section: one entry, flags 0 (min only), min 1.
        let module = module(MEMORY_SECTION, &[0x01, 0x00, 0x01]);
        let probe = probe(&module);
        assert!(!probe.shared_memory);
        assert!(!probe.fast_path_capable());
    }

    #[test]
    fn shared_memory_is_detected() {
        // Memory section: one entry, flags 0x03 (has max + shared), min 1, max 2.
        let module = module(MEMORY_SECTION, &[0x01, 0x03, 0x01, 0x02]);
        let probe = probe(&module);
        assert!(probe.shared_memory);
        assert!(!probe.fast_path_capable(), "notify opcode still required");
    }

    #[test]
    fn atomic_notify_opcode_is_detected() {
        // Code section containing an atomic.notify opcode sequence.
        let module = module(CODE_SECTION, &[0xFE, 0x00, 0x02, 0x00]);
        let probe = probe(&module);
        assert!(probe.atomic_notify);
        assert!(!probe.fast_path_capable(), "shared memory still required");
    }

    #[test]
    fn shared_module_with_notify_is_capable() {
        let mut bytes = b"\0asm\x01\x00\x00\x00".to_vec();
        // Shared memory with max.
        let memory_payload = [0x01, 0x03, 0x01, 0x02];
        bytes.push(MEMORY_SECTION);
        bytes.push(memory_payload.len() as u8);
        bytes.extend_from_slice(&memory_payload);
        // Code containing atomic.notify.
        let code_payload = [0xFE, 0x00, 0x02, 0x00];
        bytes.push(CODE_SECTION);
        bytes.push(code_payload.len() as u8);
        bytes.extend_from_slice(&code_payload);

        let probe = probe(&bytes);
        assert!(probe.fast_path_capable());
    }

    #[test]
    fn malformed_memory_section_falls_back_safely() {
        let module = module(MEMORY_SECTION, &[0xFF, 0xFF]);
        let probe = probe(&module);
        assert!(!probe.shared_memory);
    }

    /// Builds an export-section payload declaring one export with `name`.
    fn export_payload(name: &str) -> Vec<u8> {
        let mut payload = vec![0x01]; // one export
        payload.push(name.len() as u8);
        payload.extend_from_slice(name.as_bytes());
        payload.push(0x00); // kind: func
        payload.push(0x00); // index 0
        payload
    }

    #[test]
    fn worker_entry_export_is_detected() {
        let module = module(EXPORT_SECTION, &export_payload(WORKER_ENTRY_EXPORT));
        let probe = probe(&module);
        assert!(probe.worker_entry);
        assert!(probe.multithreaded());
    }

    #[test]
    fn guest_without_worker_entry_falls_back_to_cooperative() {
        let module = module(EXPORT_SECTION, &export_payload("__selium_guest_poll"));
        let probe = probe(&module);
        assert!(!probe.worker_entry);
        assert!(!probe.multithreaded());
    }

    #[test]
    fn worker_entry_detection_is_exact() {
        // A similar-but-different name must not match.
        let module = module(
            EXPORT_SECTION,
            &export_payload("__selium_guest_worker_extra"),
        );
        let probe = probe(&module);
        assert!(!probe.worker_entry);
    }

    #[test]
    fn malformed_export_section_falls_back_to_cooperative() {
        // Count claims one export but the payload ends immediately.
        let module = module(EXPORT_SECTION, &[0x01]);
        let probe = probe(&module);
        assert!(!probe.worker_entry);
        assert!(!probe.multithreaded());
    }
}

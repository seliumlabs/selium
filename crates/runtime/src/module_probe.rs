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

const CUSTOM_SECTION: u8 = 0;
const CODE_SECTION: u8 = 10;
const EXPORT_SECTION: u8 = 7;
const MEMORY_SECTION: u8 = 5;
const IMPORT_SECTION: u8 = 2;
/// The worker entry export name a multithreaded guest module must carry.
/// Presence selects multithreaded execution (dedicated worker pool); absence
/// falls back to the single-worker cooperative reactor.
pub const WORKER_ENTRY_EXPORT: &str = "__selium_guest_worker";

/// The passive ABI declaration found in a module's custom sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiVersionDeclaration {
    /// No ABI custom section was present.
    Missing,
    /// Exactly one well-formed ABI custom section was present.
    Version(u32),
    /// The ABI section was malformed or duplicated.
    Invalid,
}

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
    /// The module imports the hostcall request boundary and therefore must
    /// negotiate its ABI before instantiation proceeds to guest execution.
    pub uses_hostcalls: bool,
    /// The module's passive ABI version declaration.
    pub abi_version: AbiVersionDeclaration,
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
        uses_hostcalls: false,
        abi_version: AbiVersionDeclaration::Missing,
    };

    let Some(mut cursor) = SectionCursor::new(module) else {
        return probe;
    };

    while let Some((id, payload)) = cursor.next_section() {
        match id {
            CUSTOM_SECTION => {
                if let Some(declaration) = scan_abi_custom_section(payload) {
                    probe.abi_version = match probe.abi_version {
                        AbiVersionDeclaration::Missing => declaration,
                        AbiVersionDeclaration::Version(_) | AbiVersionDeclaration::Invalid => {
                            AbiVersionDeclaration::Invalid
                        }
                    };
                }
            }
            IMPORT_SECTION => probe.uses_hostcalls |= scan_import_section(payload),
            MEMORY_SECTION => probe.shared_memory |= scan_memory_section(payload),
            CODE_SECTION => probe.atomic_notify |= scan_code_section(payload),
            EXPORT_SECTION => probe.worker_entry |= scan_export_section(payload),
            _ => {}
        }
    }
    probe
}

/// Reads a custom section without executing the module. The Selium ABI
/// declaration payload is exactly one little-endian `u32`; unrelated custom
/// sections are ignored. `Some(Invalid)` distinguishes a malformed declaration
/// from an absent one, and duplicates are rejected by the caller.
fn scan_abi_custom_section(payload: &[u8]) -> Option<AbiVersionDeclaration> {
    let mut cursor = SectionCursor {
        bytes: payload,
        pos: 0,
    };
    let name_len = cursor.read_leb_u32()? as usize;
    let name_end = cursor.pos.checked_add(name_len)?;
    let name = cursor.bytes.get(cursor.pos..name_end)?;
    if name != selium_abi::SELIUM_ABI_VERSION_SECTION.as_bytes() {
        return None;
    }
    let version = cursor
        .bytes
        .get(name_end..)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .map_or(AbiVersionDeclaration::Invalid, |version| {
            AbiVersionDeclaration::Version(version)
        });
    Some(version)
}

/// True when the code section payload contains the `memory.atomic.notify`
/// opcode sequence (`0xFE 0x00`). See the module docs for the false-positive
/// analysis; there are no false negatives.
fn scan_code_section(payload: &[u8]) -> bool {
    payload.windows(2).any(|pair| pair == [0xFE, 0x00])
}

/// True when the import section contains the exact hostcall request import.
/// Parsing is deliberately bounded by the module bytes; malformed imports
/// return false and are subsequently rejected by the engine validator.
fn scan_import_section(payload: &[u8]) -> bool {
    fn name<'a>(cursor: &mut SectionCursor<'a>) -> Option<&'a [u8]> {
        let len = cursor.read_leb_u32()? as usize;
        let end = cursor.pos.checked_add(len)?;
        let name = cursor.bytes.get(cursor.pos..end)?;
        cursor.pos = end;
        Some(name)
    }

    fn limits(cursor: &mut SectionCursor<'_>) -> Option<()> {
        let flags = *cursor.bytes.get(cursor.pos)?;
        cursor.pos += 1;
        cursor.read_leb_u32()?;
        if flags & 1 != 0 {
            cursor.read_leb_u32()?;
        }
        Some(())
    }

    let mut cursor = SectionCursor {
        bytes: payload,
        pos: 0,
    };
    let Some(count) = cursor.read_leb_u32() else {
        return false;
    };
    for _ in 0..count {
        let (Some(module), Some(import)) = (name(&mut cursor), name(&mut cursor)) else {
            return false;
        };
        if module == b"selium" && import == b"hostcall_create" {
            return true;
        }
        let Some(kind) = cursor.bytes.get(cursor.pos).copied() else {
            return false;
        };
        cursor.pos += 1;
        let valid = match kind {
            0 => cursor.read_leb_u32().is_some(),
            1 => {
                cursor.pos += 1;
                limits(&mut cursor).is_some()
            }
            2 => limits(&mut cursor).is_some(),
            3 => {
                cursor.pos += 2;
                cursor.pos <= cursor.bytes.len()
            }
            4 => {
                cursor.pos += 1;
                cursor.read_leb_u32().is_some()
            }
            _ => false,
        };
        if !valid {
            return false;
        }
    }
    false
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

    fn abi_custom_payload(version: u32) -> Vec<u8> {
        let name = selium_abi::SELIUM_ABI_VERSION_SECTION.as_bytes();
        let mut payload = vec![name.len() as u8];
        payload.extend_from_slice(name);
        payload.extend_from_slice(&version.to_le_bytes());
        payload
    }

    #[test]
    fn abi_version_is_read_from_the_passive_custom_section() {
        let module = module(
            CUSTOM_SECTION,
            &abi_custom_payload(selium_abi::SELIUM_ABI_VERSION),
        );
        assert_eq!(
            probe(&module).abi_version,
            AbiVersionDeclaration::Version(selium_abi::SELIUM_ABI_VERSION)
        );
    }

    #[test]
    fn malformed_or_duplicate_abi_declarations_are_invalid() {
        let mut malformed = abi_custom_payload(selium_abi::SELIUM_ABI_VERSION);
        assert!(malformed.pop().is_some());
        assert_eq!(
            probe(&module(CUSTOM_SECTION, &malformed)).abi_version,
            AbiVersionDeclaration::Invalid
        );

        let payload = abi_custom_payload(selium_abi::SELIUM_ABI_VERSION);
        let mut bytes = module(CUSTOM_SECTION, &payload);
        let second = module(CUSTOM_SECTION, &payload);
        bytes.extend_from_slice(second.get(8..).expect("skip the second module header"));
        assert_eq!(probe(&bytes).abi_version, AbiVersionDeclaration::Invalid);
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
    fn hostcall_import_is_detected_exactly() {
        let module = wat::parse_str(
            r#"(module
                (import "selium" "hostcall_create" (func (param i32 i32) (result i64)))
                (memory 1))"#,
        )
        .expect("compile import fixture");
        assert!(probe(&module).uses_hostcalls);

        let module = wat::parse_str(
            r#"(module
                (import "selium" "process_id" (func (result i64)))
                (memory 1))"#,
        )
        .expect("compile non-hostcall import fixture");
        assert!(!probe(&module).uses_hostcalls);
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

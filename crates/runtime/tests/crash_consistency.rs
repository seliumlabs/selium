//! Format-level torn-write lane: the durable *record format* cut mid-write
//! must replay to a consistent state on restart.
//!
//! These tests operate on the framed record format directly (append a byte
//! prefix of a real volume, then reopen it); they deliberately do **not**
//! claim component-level behaviour. Real store interruption/restart injection
//! lives in `guests/store/src/tests.rs`; identity and control-plane component
//! interruption/restart evidence lives in `identity_durability.rs` and
//! `control_plane_durability.rs` over real DurableLog/store RPCs.
//! The store's on-disk contract is a framed record log: each record carries a
//! length and a CRC32, and a reader replays frames until the first frame that
//! is incomplete or fails its checksum (`selium_store::format::scan_frames`).
//! That is the whole crash story for the durable surfaces that sit on it — the
//! store, the identity guest's registries and logs, and the control plane's
//! recorded deployments all publish through these framed volumes — so the test
//! cuts the write at **every byte offset** of a real volume and asserts that
//! the replayed prefix is exactly the records that were complete, never a
//! partial or corrupted one.
//!
//! "Restart" is a close and a fresh open of the volume: the kernel's volume
//! layer holds no in-memory truth, so the bytes on disk are what replay sees.
//! The tear is produced by appending only a prefix to a fresh segment, which
//! is the state a crash between two writes leaves behind.
//!
//! `#[ignore]`d: the design places crash-consistency in the scheduled lane
//! rather than every push (`scripts/gate.sh --soak` runs it nightly). Unlike
//! the soak, the lane needs no wasm guests or network, so it is cheap.
//!
//! ```sh
//! cargo test -p selium-runtime --test crash_consistency -- --ignored
//! ```

use selium_kernel::{Kernel, VolumeRegistry};
use selium_store::format::{self, Record};

/// A state root removed on drop.
struct TempRoot(std::path::PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "selium-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        )))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// The manifest publish is the durable decision point for a blob: a crash
/// before it completes must leave the previous manifest as the visible state.
#[test]
#[ignore = "crash-consistency lane: run nightly via scripts/gate.sh --soak"]
fn format_torn_manifest_publish_keeps_the_previous_record() {
    let root = TempRoot::new("crash-manifest");
    let kernel = Kernel::with_state_dir(13, root.path());
    let volumes = kernel.volumes();

    let first = format::frame(&format::encode_manifest("leaf", "blob-one"));
    let second = format::frame(&format::encode_manifest("leaf", "blob-two"));
    let name = format::manifest_volume("crash-manifest");

    // Complete first publish, torn second publish.
    let mut bytes = first.clone();
    bytes.extend_from_slice(&second[..second.len() / 2]);
    let restarted = write_and_restart(&volumes, &name, &bytes);

    let (frames, end) = format::scan_frames(&restarted);
    let manifests: Vec<(String, String)> = frames
        .into_iter()
        .filter_map(|frame| format::decode_manifest(frame).ok())
        .collect();
    assert_eq!(
        manifests,
        vec![("leaf".to_string(), "blob-one".to_string())],
        "the torn publish must not replace the visible manifest"
    );
    assert_eq!(
        format::torn_tail(&restarted),
        Some(end),
        "the torn publish is exactly the tail after the last complete frame"
    );

    // A recovery path trims the torn tail and retries the publish into a
    // fresh volume (the state a successful retry leaves behind).
    let mut trimmed = restarted[..end].to_vec();
    trimmed.extend_from_slice(&second);
    let recovered_name = format::manifest_volume("crash-manifest-recovered");
    let recovered = write_and_restart(&volumes, &recovered_name, &trimmed);
    let (frames, _end) = format::scan_frames(&recovered);
    let recovered_manifests: Vec<(String, String)> = frames
        .into_iter()
        .filter_map(|frame| format::decode_manifest(frame).ok())
        .collect();
    assert_eq!(
        recovered_manifests,
        vec![
            ("leaf".to_string(), "blob-one".to_string()),
            ("leaf".to_string(), "blob-two".to_string()),
        ],
        "after trimming, a retried publish lands as the second complete frame"
    );
}

/// A crash that corrupts a record already written must stop replay before it:
/// the CRC is the boundary between "durable" and "discard".
#[test]
#[ignore = "crash-consistency lane: run nightly via scripts/gate.sh --soak"]
fn format_corrupt_record_is_discarded_not_replayed() {
    let root = TempRoot::new("crash-corrupt");
    let kernel = Kernel::with_state_dir(11, root.path());
    let volumes = kernel.volumes();

    let records: Vec<Record> = (1..=3).map(record).collect();
    let mut full = Vec::new();
    for record in &records {
        full.extend_from_slice(&format::frame(&format::encode_record(record)));
    }

    let name = format::log_volume("crash-corrupt", 0);
    let clean = write_and_restart(&volumes, &name, &full);
    let (frames, _end) = format::scan_frames(&clean);
    let third_start = frames[0].len() + frames[1].len() + 2 * 8;

    // Flip one payload byte of the third record: a crash that landed a bad
    // sector rather than a short write.
    let mut corrupt = full.clone();
    let victim = third_start + 8 + 4;
    corrupt[victim] ^= 0xFF;
    let corrupt_name = format::log_volume("crash-corrupt", 1);
    let corrupt = write_and_restart(&volumes, &corrupt_name, &corrupt);

    assert_eq!(
        replay(&corrupt),
        records[..2],
        "a checksum failure discards that record and everything after it"
    );
    assert!(
        format::torn_tail(&corrupt).is_some(),
        "a corrupt record is reported as a torn tail, not silently skipped"
    );
}

/// Cuts a three-record log at every byte offset; each restart must replay a
/// prefix whose last record is complete.
// scenario: verification-gate/soak-and-crash-consistency-coverage/restart-mid-write-recovers
#[test]
#[ignore = "crash-consistency lane: run nightly via scripts/gate.sh --soak"]
fn format_truncation_replays_a_complete_record_prefix() {
    let root = TempRoot::new("crash-consistency");
    let kernel = Kernel::with_state_dir(7, root.path());
    let volumes = kernel.volumes();

    let records: Vec<Record> = (1..=3).map(record).collect();
    let mut full = Vec::new();
    let mut frame_ends = Vec::new();
    for record in &records {
        full.extend_from_slice(&format::frame(&format::encode_record(record)));
        frame_ends.push(full.len());
    }

    // The clean write reads back exactly.
    let name = format::log_volume("crash-consistency", 0);
    let clean = write_and_restart(&volumes, &name, &full);
    assert_eq!(
        replay(&clean),
        records,
        "an uninterrupted write replays whole"
    );
    assert_eq!(format::torn_tail(&clean), None);

    for cut in 0..=full.len() {
        // Each cut is its own segment: a fresh volume whose only content is
        // the prefix a crash would have left.
        let name = format::log_volume("crash-consistency", (cut as u64) + 1);
        let torn = write_and_restart(&volumes, &name, &full[..cut]);
        assert_eq!(torn.len(), cut, "the crash left exactly {cut} bytes");

        let replayed = replay(&torn);
        // The prefix is exactly the records whose frame ended at or before the
        // cut: no partial record is ever surfaced as durable.
        let expected_len = frame_ends.iter().filter(|end| **end <= cut).count();
        assert_eq!(
            replayed,
            records[..expected_len],
            "cut {cut}: replay must be the complete prefix, not a partial record"
        );

        // Replay is idempotent: a second restart over the same bytes sees the
        // same state. (A distinct volume, because appending to the first would
        // replay the prefix twice.)
        let replay_name = format::log_volume("crash-consistency-replay", (cut as u64) + 1);
        let again = write_and_restart(&volumes, &replay_name, &torn);
        assert_eq!(
            replay(&again),
            replayed,
            "cut {cut}: replay must be deterministic"
        );

        // A torn tail is reported exactly when the cut is not frame-aligned,
        // which is what lets a recovery path trim rather than guess.
        let aligned = cut == 0 || frame_ends.contains(&cut);
        assert_eq!(
            format::torn_tail(&torn).is_some(),
            !aligned,
            "cut {cut}: torn-tail detection must match frame alignment"
        );
    }
}

/// Builds a record with a distinct sequence and payload per index.
fn record(sequence: u64) -> Record {
    Record {
        sequence,
        timestamp_ms: 1_000 + sequence,
        headers: vec![("source".to_string(), "crash-consistency".to_string())],
        payload: format!("record-{sequence}").into_bytes(),
    }
}

/// Replays every decodable record in `bytes`, as the store's reader does.
fn replay(bytes: &[u8]) -> Vec<Record> {
    let (frames, _end) = format::scan_frames(bytes);
    frames
        .into_iter()
        .filter_map(|frame| format::decode_record(frame).ok())
        .collect()
}

/// Writes `bytes` to a fresh volume under `name`, syncs, closes, and reopens —
/// the durable state a restart replays.
fn write_and_restart(volumes: &VolumeRegistry, name: &str, bytes: &[u8]) -> Vec<u8> {
    let descriptor = volumes.open(None, name).expect("open crash volume");
    if !bytes.is_empty() {
        volumes
            .append(descriptor.local_id, bytes)
            .expect("append crash bytes");
    }
    volumes
        .sync(descriptor.local_id)
        .expect("sync crash volume");
    drop(volumes.close(descriptor.local_id));

    let reopened = volumes.open(None, name).expect("reopen crash volume");
    let bytes = volumes
        .read_at(reopened.local_id, 0, u32::MAX as usize)
        .expect("read crash volume");
    drop(volumes.close(reopened.local_id));
    bytes
}

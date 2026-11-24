//! Native tests for the durable store's semantics and recovery.
//!
//! The tests run the real [`Store`] over an in-memory volume layer, which can
//! be truncated behind the store's back to simulate a power cycle mid-append.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::{
    format,
    store::{Failure, Store, VolumeHandle, Volumes},
};

/// Kind of volume key: `(tenant, name)`.
type VolumeKey = (String, String);

#[derive(Clone, Default)]
struct MemVolumes {
    files: Arc<Mutex<HashMap<VolumeKey, Vec<u8>>>>,
    fail_append: Arc<std::sync::atomic::AtomicBool>,
    fail_sync: Arc<std::sync::atomic::AtomicBool>,
}

struct MemVolume {
    files: Arc<Mutex<HashMap<VolumeKey, Vec<u8>>>>,
    key: VolumeKey,
    fail_append: Arc<std::sync::atomic::AtomicBool>,
    fail_sync: Arc<std::sync::atomic::AtomicBool>,
}

impl MemVolumes {
    fn key(tenant: Option<&str>, name: &str) -> VolumeKey {
        (tenant.unwrap_or_default().to_string(), name.to_string())
    }

    /// Locks the shared volume map, surfacing poisoning as a store failure
    /// (these helpers are used from `Result`-returning trait methods).
    fn files(&self) -> Result<std::sync::MutexGuard<'_, HashMap<VolumeKey, Vec<u8>>>, Failure> {
        self.files.lock().map_err(|_error| Failure {
            code: "Host",
            message: "test volume lock poisoned".to_string(),
        })
    }

    /// Truncates a volume behind the store's back (a torn append).
    fn truncate(&self, tenant: Option<&str>, name: &str, len: usize) {
        let mut files = self.files.lock().expect("lock");
        if let Some(bytes) = files.get_mut(&Self::key(tenant, name)) {
            bytes.truncate(len);
        }
    }

    /// Appends raw bytes to a volume behind the store's back (a torn tail).
    fn append_raw(&self, tenant: Option<&str>, name: &str, bytes: &[u8]) {
        self.files
            .lock()
            .expect("lock")
            .entry(Self::key(tenant, name))
            .or_default()
            .extend_from_slice(bytes);
    }

    fn volume_len(&self, tenant: Option<&str>, name: &str) -> usize {
        self.files
            .lock()
            .expect("lock")
            .get(&Self::key(tenant, name))
            .map(Vec::len)
            .unwrap_or(0)
    }

    fn volume_bytes(&self, tenant: Option<&str>, name: &str) -> Vec<u8> {
        self.files
            .lock()
            .expect("lock")
            .get(&Self::key(tenant, name))
            .cloned()
            .unwrap_or_default()
    }
}

impl MemVolumes {
    fn set_fail_append(&self, fail: bool) {
        self.fail_append
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    fn set_fail_sync(&self, fail: bool) {
        self.fail_sync
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Volumes for MemVolumes {
    type Handle = MemVolume;

    fn list(&self, tenant: Option<&str>) -> Result<Vec<String>, Failure> {
        let wanted = tenant.unwrap_or_default().to_string();
        let mut names: Vec<String> = self
            .files()?
            .keys()
            .filter(|(key_tenant, _name)| key_tenant == &wanted)
            .map(|(_tenant, name)| name.clone())
            .collect();
        names.sort();
        Ok(names)
    }

    fn open(&self, tenant: Option<&str>, name: &str) -> Result<Self::Handle, Failure> {
        let key = Self::key(tenant, name);
        self.files()?.entry(key.clone()).or_default();
        Ok(MemVolume {
            files: Arc::clone(&self.files),
            key,
            fail_append: Arc::clone(&self.fail_append),
            fail_sync: Arc::clone(&self.fail_sync),
        })
    }

    fn remove(&self, tenant: Option<&str>, name: &str) -> Result<u64, Failure> {
        let key = Self::key(tenant, name);
        let mut files = self.files()?;
        let Some(bytes) = files.remove(&key) else {
            return Err(Failure {
                code: "NotFound",
                message: format!("volume {name:?}"),
            });
        };
        Ok(bytes.len() as u64)
    }
}

impl MemVolume {
    /// Locks the shared volume map, surfacing poisoning as a store failure.
    fn files(&self) -> Result<std::sync::MutexGuard<'_, HashMap<VolumeKey, Vec<u8>>>, Failure> {
        self.files.lock().map_err(|_error| Failure {
            code: "Host",
            message: "test volume lock poisoned".to_string(),
        })
    }
}

impl VolumeHandle for MemVolume {
    fn append(&self, bytes: &[u8]) -> Result<u64, Failure> {
        if self.fail_append.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Failure {
                code: "Host",
                message: "injected append failure".to_string(),
            });
        }
        let mut files = self.files()?;
        let file = files.entry(self.key.clone()).or_default();
        let offset = file.len() as u64;
        file.extend_from_slice(bytes);
        Ok(offset)
    }

    fn read_all(&self) -> Result<Vec<u8>, Failure> {
        Ok(self.files()?.get(&self.key).cloned().unwrap_or_default())
    }

    fn len(&self) -> Result<u64, Failure> {
        Ok(self.read_all()?.len() as u64)
    }

    fn sync(&self) -> Result<(), Failure> {
        if self.fail_sync.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Failure {
                code: "Host",
                message: "injected sync failure".to_string(),
            });
        }
        Ok(())
    }
}
// scenario: selium-store/crash-consistent-recovery/torn-tail-record-discarded

#[test]
fn a_torn_tail_is_discarded_and_sequence_numbering_resumes() {
    let (volumes, store) = store();
    append(&store, "orders", b"first");
    append(&store, "orders", b"second");
    let segment = format::log_volume("orders", 0);
    let complete = volumes.volume_len(Some("acme"), &segment);

    // A partially written record lands at the tail, then the host restarts.
    volumes.append_raw(Some("acme"), &segment, &[0x40, 0x00, 0x00, 0x00, 0x01]);
    assert!(volumes.volume_len(Some("acme"), &segment) > complete);

    let restarted = Store::new(volumes.clone());
    let records = restarted
        .replay("acme", "orders", 0, u32::MAX)
        .expect("replay");
    assert_eq!(records.len(), 2, "only the complete records are replayed");

    // The next append continues after the last complete sequence, in a fresh
    // segment that leaves the torn bytes behind.
    let sequence = restarted
        .append("acme", "orders", 7, Vec::new(), b"third".to_vec())
        .expect("append after recovery");
    assert_eq!(sequence, 3);
    let records = restarted
        .replay("acme", "orders", 0, u32::MAX)
        .expect("replay");
    assert_eq!(records.len(), 3);
    assert_eq!(records[2].payload, b"third".to_vec());
    let segment = format::log_volume("orders", 1);
    assert!(volumes.volume_len(Some("acme"), &segment) > 0);
}

#[test]
fn a_truncated_volume_mid_record_keeps_the_complete_prefix() {
    let (volumes, store) = store();
    append(&store, "orders", b"first");
    append(&store, "orders", b"second");
    let segment = format::log_volume("orders", 0);
    let complete = volumes.volume_len(Some("acme"), &segment);
    // Cut the volume in the middle of the second record's frame.
    volumes.truncate(Some("acme"), &segment, complete - 3);

    let restarted = Store::new(volumes);
    let records = restarted
        .replay("acme", "orders", 0, u32::MAX)
        .expect("replay");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].payload, b"first".to_vec());
    assert_eq!(
        restarted
            .append("acme", "orders", 7, Vec::new(), b"second".to_vec())
            .expect("append"),
        2
    );
}
// scenario: selium-store/durable-slice-resolution/manifest-resolves-to-a-slice
// scenario: selium-store/durable-slice-resolution/absent-blob-cannot-be-resolved

#[test]
fn an_absent_blob_or_manifest_resolves_to_no_slice() {
    let (_volumes, store) = store();
    let blob_id = store
        .put_blob("acme", "assets", b"module-bytes")
        .expect("put");

    // A blob id the store does not hold.
    assert_eq!(
        store
            .resolve_slice("acme", "assets", "no-such-blob")
            .expect("resolve absent"),
        None,
        "an absent blob cannot be resolved"
    );
    // A blob that belongs to another store's namespace is not resolvable
    // through this one.
    assert_eq!(
        store
            .resolve_slice("acme", "other-store", &blob_id)
            .expect("resolve absent store"),
        None,
        "a blob outside the named store resolves to nothing"
    );
    // A manifest the store has not recorded.
    assert_eq!(
        store
            .get_manifest("acme", "assets", "latest")
            .expect("manifest"),
        None,
        "an absent manifest cannot be resolved"
    );
    // Once recorded, the manifest's blob resolves to a slice.
    store
        .set_manifest("acme", "assets", "latest", &blob_id)
        .expect("manifest");
    let id = store
        .get_manifest("acme", "assets", "latest")
        .expect("manifest")
        .expect("recorded");
    assert!(
        store
            .resolve_slice("acme", "assets", &id)
            .expect("resolve manifest")
            .is_some(),
        "a manifest resolves to the slice covering its blob"
    );
}

fn append(store: &Store<MemVolumes>, log: &str, payload: &[u8]) -> u64 {
    store
        .append("acme", log, 7, Vec::new(), payload.to_vec())
        .expect("append")
}
// scenario: selium-store/durable-log-semantics/append-and-replay

#[test]
fn append_and_replay_are_ordered() {
    let (_volumes, store) = store();
    assert_eq!(append(&store, "orders", b"first"), 1);
    assert_eq!(append(&store, "orders", b"second"), 2);

    let records = store.replay("acme", "orders", 0, u32::MAX).expect("replay");
    let payloads: Vec<&[u8]> = records
        .iter()
        .map(|record| record.payload.as_slice())
        .collect();
    assert_eq!(payloads, vec![b"first".as_slice(), b"second".as_slice()]);
    assert_eq!(
        records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );

    // Replay from a sequence and with a bound.
    let tail = store.replay("acme", "orders", 2, 10).expect("replay tail");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].payload, b"second".to_vec());
    let bounded = store
        .replay("acme", "orders", 0, 1)
        .expect("bounded replay");
    assert_eq!(bounded.len(), 1);
}
// scenario: selium-store/blob-store-semantics/put-and-get-round-trip
// scenario: selium-store/blob-store-semantics/manifest-for-an-absent-blob-rejected

#[test]
fn blob_put_get_and_manifest_round_trip() {
    let (_volumes, store) = store();
    let blob_id = store.put_blob("acme", "assets", b"module").expect("put");
    assert_eq!(blob_id, format::blob_id(b"module"));
    assert_eq!(
        store.get_blob("acme", "assets", &blob_id).expect("get"),
        Some(b"module".to_vec())
    );
    assert_eq!(
        store
            .get_blob("acme", "assets", "deadbeef")
            .expect("get missing"),
        None
    );

    store
        .set_manifest("acme", "assets", "latest", &blob_id)
        .expect("set manifest");
    assert_eq!(
        store
            .get_manifest("acme", "assets", "latest")
            .expect("get manifest"),
        Some(blob_id)
    );
    let failure = store
        .set_manifest("acme", "assets", "broken", "deadbeef")
        .expect_err("absent blob rejected");
    assert_eq!(failure.code, "UnknownBlob");
}
// scenario: selium-store/durable-slice-resolution/cross-tenant-resolution-excluded
// scenario: selium-store/slice-stability/a-slice-never-goes-stale

#[test]
fn blob_slice_is_stable_across_further_writes() {
    let (volumes, store) = store();
    let blob_id = store
        .put_blob("acme", "assets", b"module-bytes")
        .expect("put");
    store
        .set_manifest("acme", "assets", "latest", &blob_id)
        .expect("manifest");

    let (volume, len) = store
        .resolve_slice("acme", "assets", &blob_id)
        .expect("resolve")
        .expect("slice present");
    assert_eq!(len, b"module-bytes".len() as u64);
    let before = volumes.volume_bytes(Some("acme"), &volume);
    assert_eq!(before, b"module-bytes");

    // Further blob writes append new volumes; the resolved slice's bytes are
    // never rewritten in place.
    store
        .put_blob("acme", "assets", b"other")
        .expect("put other");
    store.put_blob("acme", "assets", b"more").expect("put more");
    let after = volumes.volume_bytes(Some("acme"), &volume);
    assert_eq!(before, after, "a slice never goes stale");

    // Resolution is tenant-scoped: another tenant sees no slice.
    assert_eq!(
        store
            .resolve_slice("beta", "assets", &blob_id)
            .expect("resolve cross-tenant"),
        None
    );
}
// scenario: selium-store/durable-log-semantics/checkpoint-references-a-missing-sequence

#[test]
fn checkpoint_references_an_existing_sequence() {
    let (_volumes, store) = store();
    let sequence = append(&store, "orders", b"first");
    store
        .write_checkpoint("acme", "orders", "boot", sequence)
        .expect("checkpoint");
    assert_eq!(
        store
            .read_checkpoint("acme", "orders", "boot")
            .expect("read checkpoint"),
        Some(sequence)
    );
    assert_eq!(
        store
            .read_checkpoint("acme", "orders", "missing")
            .expect("read missing"),
        None
    );
    assert_eq!(
        store
            .write_checkpoint("acme", "orders", "broken", sequence + 5)
            .expect_err("missing sequence rejected")
            .code,
        "NotFound"
    );
}
// scenario: selium-store/tenant-scoped-volume-layout/cross-tenant-read-is-excluded

#[test]
fn cross_tenant_blob_read_is_excluded() {
    let (_volumes, store) = store();
    let blob_id = store.put_blob("acme", "assets", b"secret").expect("put");
    assert_eq!(
        store
            .get_blob("acme", "assets", &blob_id)
            .expect("acme get"),
        Some(b"secret".to_vec())
    );
    // Tenant `beta` sees no such blob, and the same id cannot be read there.
    assert_eq!(
        store
            .get_blob("beta", "assets", &blob_id)
            .expect("beta get"),
        None
    );
    assert_eq!(
        store
            .get_manifest("beta", "assets", "latest")
            .expect("beta manifest"),
        None
    );
}

#[test]
fn format_round_trips_records_and_volume_names() {
    let record = format::Record {
        sequence: 9,
        timestamp_ms: 11,
        headers: vec![("kind".to_string(), "test".to_string())],
        payload: b"payload".to_vec(),
    };
    let framed = format::frame(&format::encode_record(&record));
    let (bodies, end) = format::scan_frames(&framed);
    assert_eq!(end, framed.len());
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        format::decode_record(bodies[0]).expect("decode"),
        record.clone()
    );

    // A corrupted frame is not surfaced.
    let mut corrupted = framed.clone();
    corrupted[0] ^= 0xff;
    let (bodies, end) = format::scan_frames(&corrupted);
    assert!(bodies.is_empty());
    assert_eq!(end, 0);

    let name = "with/slash and space";
    assert_eq!(
        format::parse_log_volume(&format::log_volume(name, 3))
            .expect("log volume")
            .expect("decode"),
        (name.to_string(), 3)
    );
    assert_eq!(
        format::parse_blob_volume(&format::blob_volume("store", "abc123"))
            .expect("blob volume")
            .expect("decode"),
        ("store".to_string(), "abc123".to_string())
    );
    assert_eq!(
        format::parse_blob_volume(&format::manifest_volume("store")),
        None
    );
    assert_eq!(
        format::parse_checkpoint_volume(&format::checkpoint_volume("orders"))
            .expect("checkpoint volume")
            .expect("decode"),
        "orders".to_string()
    );
}
// scenario: selium-store/tenant-scoped-volume-layout/same-name-in-two-tenants-is-distinct

#[test]
fn identical_names_in_two_tenants_are_distinct() {
    let (_volumes, store) = store();
    assert_eq!(append(&store, "orders", b"acme-1"), 1);
    let beta = store
        .append("beta", "orders", 7, Vec::new(), b"beta-1".to_vec())
        .expect("beta append");
    assert_eq!(beta, 1, "beta's log has its own sequence space");

    let acme = store.replay("acme", "orders", 0, u32::MAX).expect("acme");
    let beta = store.replay("beta", "orders", 0, u32::MAX).expect("beta");
    assert_eq!(acme[0].payload, b"acme-1".to_vec());
    assert_eq!(beta[0].payload, b"beta-1".to_vec());
}

fn store() -> (MemVolumes, Store<MemVolumes>) {
    let volumes = MemVolumes::default();
    let store = Store::new(volumes.clone());
    (volumes, store)
}
// scenario: selium-store/crash-consistent-recovery/synced-state-survives-restart

#[test]
fn synced_state_survives_a_store_restart() {
    let (volumes, store) = store();
    let sequence = append(&store, "orders", b"durable");
    store
        .write_checkpoint("acme", "orders", "boot", sequence)
        .expect("checkpoint");
    let blob_id = store.put_blob("acme", "assets", b"module").expect("put");
    store
        .set_manifest("acme", "assets", "latest", &blob_id)
        .expect("manifest");

    // A fresh store over the same volumes is what a restart produces.
    let restarted = Store::new(volumes);
    let records = restarted
        .replay("acme", "orders", 0, u32::MAX)
        .expect("replay");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].payload, b"durable".to_vec());
    assert_eq!(
        restarted
            .read_checkpoint("acme", "orders", "boot")
            .expect("checkpoint"),
        Some(sequence)
    );
    assert_eq!(
        restarted.get_blob("acme", "assets", &blob_id).expect("get"),
        Some(b"module".to_vec())
    );
    assert_eq!(
        restarted
            .get_manifest("acme", "assets", "latest")
            .expect("manifest"),
        Some(blob_id)
    );
    assert_eq!(
        restarted
            .append("acme", "orders", 7, Vec::new(), b"next".to_vec())
            .expect("append"),
        2
    );
}

// scenario: verification-gate/crash-injection-stops-durable-components-mid-write/store-stops-between-append-and-manifest-publication
#[test]
fn crash_between_blob_append_and_manifest_publish_exposes_only_acknowledged_state() {
    let (volumes, store) = store();
    let id = store
        .put_blob("acme", "assets", b"module")
        .expect("append the blob");

    // Crash here: the blob append committed, the manifest publish never ran.
    let restarted = Store::new(volumes);
    assert_eq!(
        restarted.get_blob("acme", "assets", &id).expect("get"),
        Some(b"module".to_vec()),
        "the acknowledged append survives the restart"
    );
    assert_eq!(
        restarted
            .get_manifest("acme", "assets", "latest")
            .expect("manifest"),
        None,
        "a manifest that was never published must not appear after restart"
    );

    // The publish retried after restart becomes the visible manifest.
    restarted
        .set_manifest("acme", "assets", "latest", &id)
        .expect("manifest");
    assert_eq!(
        restarted
            .get_manifest("acme", "assets", "latest")
            .expect("manifest"),
        Some(id)
    );
}

// scenario: selium-store/recovery-precedes-every-first-blob-mutation/put-is-the-first-operation-after-restart
// scenario: selium-store/recovered-blob-content-matches-its-identity/existing-content-is-put-again
#[test]
fn put_as_first_operation_after_restart_recovers_existing_state() {
    let volumes = MemVolumes::default();
    let store = Store::new(volumes.clone());
    let existing = store
        .put_blob("acme", "assets", b"existing")
        .expect("put existing");
    store
        .set_manifest("acme", "assets", "latest", &existing)
        .expect("manifest");
    let volume = format::blob_volume("assets", &existing);
    let before_len = volumes.volume_len(Some("acme"), &volume);

    // The first operation after restart is a put: recovery must precede it.
    let restarted = Store::new(volumes.clone());
    let new_id = restarted
        .put_blob("acme", "assets", b"new")
        .expect("put new");
    assert_eq!(
        restarted
            .get_blob("acme", "assets", &existing)
            .expect("get"),
        Some(b"existing".to_vec()),
        "a pre-existing blob remains resolvable after a first-op put"
    );
    assert_eq!(
        restarted
            .get_manifest("acme", "assets", "latest")
            .expect("manifest"),
        Some(existing.clone()),
        "pre-existing manifests survive a first-op put"
    );
    assert_eq!(
        restarted.get_blob("acme", "assets", &new_id).expect("get"),
        Some(b"new".to_vec())
    );

    // Putting existing content again validates and acknowledges it without a
    // second append or a changed slice.
    assert_eq!(
        restarted
            .put_blob("acme", "assets", b"existing")
            .expect("duplicate put"),
        existing
    );
    assert_eq!(
        volumes.volume_len(Some("acme"), &volume),
        before_len,
        "duplicate put must not append a second copy"
    );
}

// scenario: selium-store/recovered-blob-content-matches-its-identity/torn-blob-append
#[test]
fn torn_tail_preserves_the_valid_acknowledged_blob_prefix() {
    let volumes = MemVolumes::default();
    let store = Store::new(volumes.clone());
    let id = store
        .put_blob("acme", "assets", b"payload")
        .expect("put payload");
    store
        .set_manifest("acme", "assets", "latest", &id)
        .expect("manifest");
    let volume = format::blob_volume("assets", &id);
    let acknowledged_len = volumes.volume_len(Some("acme"), &volume);

    // Simulate an interrupted append after the committed bytes.
    volumes.append_raw(Some("acme"), &volume, b"torn-tail");
    let recovered_len = volumes.volume_len(Some("acme"), &volume);

    let restarted = Store::new(volumes.clone());
    assert_eq!(
        restarted.get_blob("acme", "assets", &id).expect("get"),
        Some(b"payload".to_vec()),
        "recovery exposes exactly the valid prefix, never the torn tail"
    );
    assert_eq!(
        restarted
            .resolve_slice("acme", "assets", &id)
            .expect("slice"),
        Some((volume.clone(), acknowledged_len as u64)),
        "the slice is bounded to the acknowledged prefix"
    );
    assert_eq!(
        restarted
            .get_manifest("acme", "assets", "latest")
            .expect("manifest"),
        Some(id.clone()),
        "a manifest to the valid prefix remains coherent"
    );
    assert_eq!(
        volumes.volume_bytes(Some("acme"), &volume),
        [b"payload".as_slice(), b"torn-tail".as_slice()].concat(),
        "recovery does not delete previously acknowledged bytes"
    );

    restarted
        .put_blob("acme", "assets", b"payload")
        .expect("retry the acknowledged put");
    assert_eq!(
        volumes.volume_len(Some("acme"), &volume),
        recovered_len,
        "retry validates the prefix without appending another payload"
    );
}

#[test]
fn partial_first_blob_append_is_hidden_and_corrected_without_duplication() {
    let volumes = MemVolumes::default();
    let payload = b"complete payload";
    let id = format::blob_id(payload);
    let volume = format::blob_volume("assets", &id);
    volumes.append_raw(Some("acme"), &volume, &payload[..5]);

    let store = Store::new(volumes.clone());
    assert_eq!(
        store.get_blob("acme", "assets", &id).expect("get"),
        None,
        "a partial first append is not a valid blob"
    );
    assert_eq!(
        store.put_blob("acme", "assets", payload).expect("retry"),
        id
    );
    assert_eq!(
        volumes.volume_bytes(Some("acme"), &volume),
        payload,
        "the corrected put replaces only the unacknowledged partial bytes"
    );
    assert_eq!(
        store.get_blob("acme", "assets", &id).expect("get"),
        Some(payload.to_vec())
    );
}

#[test]
fn manifest_pointing_at_a_torn_blob_does_not_replace_the_valid_mapping() {
    let volumes = MemVolumes::default();
    let store = Store::new(volumes.clone());
    let valid_id = store
        .put_blob("acme", "assets", b"valid")
        .expect("put valid");
    store
        .set_manifest("acme", "assets", "latest", &valid_id)
        .expect("valid manifest");

    let torn_payload = b"torn replacement";
    let torn_id = format::blob_id(torn_payload);
    let torn_volume = format::blob_volume("assets", &torn_id);
    volumes.append_raw(Some("acme"), &torn_volume, &torn_payload[..4]);
    let manifest = format::frame(&format::encode_manifest("latest", &torn_id));
    volumes.append_raw(Some("acme"), &format::manifest_volume("assets"), &manifest);

    let restarted = Store::new(volumes);
    assert_eq!(
        restarted
            .get_manifest("acme", "assets", "latest")
            .expect("manifest"),
        Some(valid_id.clone()),
        "the unresolvable replacement does not shadow the valid mapping"
    );
    assert_eq!(
        restarted
            .get_blob("acme", "assets", &valid_id)
            .expect("valid blob"),
        Some(b"valid".to_vec())
    );
    assert_eq!(
        restarted
            .get_blob("acme", "assets", &torn_id)
            .expect("torn blob"),
        None
    );
}

#[test]
fn sync_failure_retry_validates_and_syncs_without_a_duplicate_append() {
    let volumes = MemVolumes::default();
    let store = Store::new(volumes.clone());
    let payload = b"append succeeded before sync failed";
    let id = format::blob_id(payload);
    let volume = format::blob_volume("assets", &id);

    volumes.set_fail_sync(true);
    store
        .put_blob("acme", "assets", payload)
        .expect_err("the first sync failure is returned");
    assert_eq!(
        volumes.volume_bytes(Some("acme"), &volume),
        payload,
        "the fault occurs after the append"
    );

    volumes.set_fail_sync(false);
    assert_eq!(
        store.put_blob("acme", "assets", payload).expect("retry"),
        id
    );
    assert_eq!(
        volumes.volume_bytes(Some("acme"), &volume),
        payload,
        "retry does not append a duplicate payload"
    );
    assert_eq!(
        store.get_blob("acme", "assets", &id).expect("get"),
        Some(payload.to_vec()),
        "the retried put is acknowledged only after validating content"
    );

    let restarted = Store::new(volumes);
    assert_eq!(
        restarted.get_blob("acme", "assets", &id).expect("get"),
        Some(payload.to_vec())
    );
}

#[test]
fn injected_append_failure_preserves_prior_acknowledged_state() {
    let (volumes, store) = store();
    append(&store, "orders", b"first");

    volumes.set_fail_append(true);
    assert!(
        store
            .append("acme", "orders", 2, Vec::new(), b"unacknowledged".to_vec())
            .is_err(),
        "the injected append failure must reach the caller"
    );
    volumes.set_fail_append(false);

    let restarted = Store::new(volumes.clone());
    let records = restarted
        .replay("acme", "orders", 0, u32::MAX)
        .expect("replay");
    assert_eq!(
        records.len(),
        1,
        "restart exposes only the acknowledged prefix"
    );
    assert_eq!(
        restarted
            .append("acme", "orders", 3, Vec::new(), b"retry".to_vec())
            .expect("retry append"),
        2,
        "the failed append did not consume a sequence number"
    );
}

#[test]
fn injected_sync_failure_is_reported_for_a_checkpoint() {
    let (volumes, store) = store();
    append(&store, "orders", b"first");
    volumes.set_fail_sync(true);
    assert!(
        store
            .write_checkpoint("acme", "orders", "acked", 1)
            .is_err(),
        "a failed sync must not be acknowledged"
    );
    volumes.set_fail_sync(false);
}

/// The explicit barrier is what lets a policy guest acknowledge only durable
/// state: a failed sync is reported, and a corrected retry succeeds.
#[test]
fn injected_sync_failure_is_reported_for_an_appended_record() {
    let (volumes, store) = store();
    let sequence = append(&store, "orders", b"policy");
    volumes.set_fail_sync(true);
    assert!(
        store.sync_log("acme", "orders").is_err(),
        "an append that cannot be synced must not be acknowledged as durable"
    );
    volumes.set_fail_sync(false);
    store
        .sync_log("acme", "orders")
        .expect("the corrected retry commits the same record");

    // The record is intact after the failed-then-retried barrier.
    let restarted = Store::new(volumes);
    let records = restarted
        .replay("acme", "orders", 0, u32::MAX)
        .expect("replay");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].sequence, sequence);
    assert_eq!(records[0].payload, b"policy".to_vec());
}

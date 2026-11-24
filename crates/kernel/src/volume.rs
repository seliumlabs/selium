use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use selium_abi::LocalResourceId;

use crate::{Error, Result};

/// Directory (under the state root) holding per-tenant volume files.
const VOLUMES_DIR: &str = "volumes";

/// Descriptor for an opened durable volume handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeDescriptor {
    /// Local handle id for the opened volume.
    pub local_id: LocalResourceId,
    /// Tenant the volume is recorded under (`None` is the platform tenant).
    pub tenant: Option<String>,
    /// Volume name (the logical name, not the encoded on-disk component).
    pub name: String,
    /// Current length in bytes.
    pub len: u64,
}

/// A single open local handle onto a volume file.
struct VolumeHandle {
    file: Mutex<File>,
    /// Shared append synchronisation for this underlying volume. Every handle
    /// opened on the same path holds the same lock, so offset lookup and the
    /// write are one ordered operation across handles.
    append: Arc<Mutex<()>>,
}

/// Registry of opaque durable byte containers rooted at a state directory.
///
/// The kernel stores and returns bytes without interpreting them: record
/// framing, sequence numbers, content addressing, manifests and checkpoints
/// live entirely in the `selium-store` system guest. The host's only layout
/// knowledge is `<state>/volumes/<enc tenant>/<enc name>`.
#[derive(Clone)]
pub struct VolumeRegistry {
    pub(crate) inner: Arc<VolumeRegistryInner>,
}

pub(crate) struct VolumeRegistryInner {
    root: Mutex<PathBuf>,
    /// When set, the state root is a throwaway directory removed on drop.
    ephemeral: AtomicBool,
    next_local: AtomicU64,
    handles: Mutex<HashMap<LocalResourceId, Arc<VolumeHandle>>>,
    /// Per-underlying-volume append locks, shared by every open handle.
    ///
    /// Weak on purpose: a path's lock lives exactly as long as some handle
    /// references it, and dead entries are pruned on the next open so opening
    /// short-lived tenant volumes does not grow the map for the life of the
    /// process.
    append_locks: Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
}

impl VolumeRegistry {
    /// Creates a registry rooted at `root`. The directory is created lazily on
    /// first use, so construction never fails.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::rooted(root.into(), false)
    }

    /// Creates a registry rooted at a throwaway directory removed on drop.
    pub fn ephemeral() -> Self {
        Self::rooted(unique_temp_root(), true)
    }

    fn rooted(root: PathBuf, ephemeral: bool) -> Self {
        Self {
            inner: Arc::new(VolumeRegistryInner {
                root: Mutex::new(root),
                ephemeral: AtomicBool::new(ephemeral),
                next_local: AtomicU64::new(1),
                handles: Mutex::new(HashMap::new()),
                append_locks: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Returns the state root the registry is rooted at.
    pub fn root(&self) -> PathBuf {
        self.inner.root.lock().clone()
    }

    /// Roots the volume layer at `root`.
    ///
    /// Legal only before first use — while no handle is open and no volume
    /// exists under the current root. The state root identifies the durable
    /// layer: moving it once a volume has been created or opened would leave
    /// live handles writing to one directory while new opens land in another,
    /// and would desynchronise the tenant usage derived by walking the layout
    /// from the bytes actually written. A registry built with
    /// [`VolumeRegistry::ephemeral`] discards its throwaway directory when the
    /// root moves, which is safe precisely because it is still empty.
    pub fn set_root(&self, root: impl Into<PathBuf>) -> Result<()> {
        let root = root.into();
        let mut current = self.inner.root.lock();
        if *current == root {
            return Ok(());
        }
        if !self.inner.handles.lock().is_empty() {
            return Err(Error::Invalid(
                "volume layer already holds open handles; the state root can only be set before first use"
                    .to_string(),
            ));
        }
        if dir_has_files(&current.join(VOLUMES_DIR))? {
            return Err(Error::Invalid(
                "volume layer already holds volumes; the state root can only be set before first use"
                    .to_string(),
            ));
        }
        if self.inner.ephemeral.swap(false, Ordering::SeqCst) {
            // Nothing to lose: the throwaway directory is still empty.
            drop(std::fs::remove_dir_all(&*current));
        }
        *current = root;
        Ok(())
    }

    fn root_path(&self) -> PathBuf {
        self.inner.root.lock().clone()
    }

    /// Opens or creates a volume under `tenant` and returns a handle.
    pub fn open(&self, tenant: Option<&str>, name: &str) -> Result<VolumeDescriptor> {
        let encoded = encode_name(name)?;
        let tenant_dir = encode_tenant(tenant)?;
        let dir = self.root_path().join(VOLUMES_DIR).join(&tenant_dir);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(encoded);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let len = file.metadata()?.len();
        let local_id = self.inner.next_local.fetch_add(1, Ordering::SeqCst);
        let append = {
            let mut locks = self.inner.append_locks.lock();
            // Drop locks whose last handle has closed; otherwise opening
            // transient volumes would accumulate one entry per path forever.
            locks.retain(|_, lock| lock.strong_count() > 0);
            let existing = locks.get(&path).and_then(Weak::upgrade);
            match existing {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(path, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        let handle = Arc::new(VolumeHandle {
            file: Mutex::new(file),
            append,
        });
        self.inner.handles.lock().insert(local_id, handle);
        Ok(VolumeDescriptor {
            local_id,
            tenant: tenant.map(str::to_string),
            name: name.to_string(),
            len,
        })
    }

    /// Appends bytes to an open volume and returns the offset they landed at.
    ///
    /// Handles opened on the same underlying volume share one append lock, so
    /// the offset lookup and the write are atomic across handles: concurrent
    /// appends never return overlapping ranges.
    pub fn append(&self, local_id: LocalResourceId, bytes: &[u8]) -> Result<u64> {
        let handle = self.handle(local_id)?;
        let _append = handle.append.lock();
        let mut file = handle.file.lock();
        let offset = file.metadata()?.len();
        file.write_all(bytes)?;
        Ok(offset)
    }

    /// Reads up to `len` bytes at `offset` from an open volume.
    pub fn read_at(&self, local_id: LocalResourceId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let handle = self.handle(local_id)?;
        let mut file = handle.file.lock();
        let total = file.metadata()?.len();
        if offset >= total {
            return Ok(Vec::new());
        }
        let available = usize::try_from(total - offset).unwrap_or(usize::MAX);
        let to_read = len.min(available);
        let mut buffer = vec![0u8; to_read];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    /// Returns the current length of an open volume.
    pub fn len(&self, local_id: LocalResourceId) -> Result<u64> {
        let handle = self.handle(local_id)?;
        let file = handle.file.lock();
        Ok(file.metadata()?.len())
    }

    /// Returns whether the volume is empty.
    pub fn is_empty(&self, local_id: LocalResourceId) -> Result<bool> {
        Ok(self.len(local_id)? == 0)
    }

    /// Lists the volume names recorded under `tenant`.
    pub fn list(&self, tenant: Option<&str>) -> Result<Vec<String>> {
        let dir = self
            .root_path()
            .join(VOLUMES_DIR)
            .join(encode_tenant(tenant)?);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let raw = entry.file_name();
            let Some(encoded) = raw.to_str() else {
                continue;
            };
            names.push(decode_component(encoded)?);
        }
        names.sort();
        Ok(names)
    }

    /// Flushes an open volume to stable storage (fsync barrier).
    pub fn sync(&self, local_id: LocalResourceId) -> Result<()> {
        let handle = self.handle(local_id)?;
        let file = handle.file.lock();
        file.sync_all()?;
        Ok(())
    }

    /// Removes a volume under `tenant`, returning the bytes it occupied.
    pub fn remove(&self, tenant: Option<&str>, name: &str) -> Result<u64> {
        let encoded = encode_name(name)?;
        let dir = self
            .root_path()
            .join(VOLUMES_DIR)
            .join(encode_tenant(tenant)?);
        let path = dir.join(encoded);
        let len = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("volume {name}")));
            }
            Err(error) => return Err(error.into()),
        };
        std::fs::remove_file(&path)?;
        Ok(len)
    }

    /// Closes a local volume handle.
    pub fn close(&self, local_id: LocalResourceId) -> Result<()> {
        self.inner
            .handles
            .lock()
            .remove(&local_id)
            .map(|_handle| ())
            .ok_or_else(|| Error::NotFound(format!("volume {local_id}")))
    }

    /// Returns the total resident bytes held by `tenant`'s volumes.
    pub fn tenant_usage(&self, tenant: Option<&str>) -> Result<u64> {
        let dir = self
            .root_path()
            .join(VOLUMES_DIR)
            .join(encode_tenant(tenant)?);
        dir_bytes(&dir)
    }

    /// Returns the resident bytes held by every tenant with volumes.
    ///
    /// Used to reconstruct per-tenant storage usage at boot by walking the
    /// state directory rather than trusting a persisted host index.
    pub fn usage_by_tenant(&self) -> Result<Vec<(String, u64)>> {
        let volumes = self.root_path().join(VOLUMES_DIR);
        let entries = match std::fs::read_dir(&volumes) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut usage = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let raw = entry.file_name();
            let Some(encoded) = raw.to_str() else {
                continue;
            };
            let tenant = decode_tenant(encoded)?;
            usage.push((tenant, dir_bytes(&entry.path())?));
        }
        usage.sort();
        Ok(usage)
    }

    fn handle(&self, local_id: LocalResourceId) -> Result<Arc<VolumeHandle>> {
        self.inner
            .handles
            .lock()
            .get(&local_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("volume {local_id}")))
    }
}

impl Drop for VolumeRegistryInner {
    fn drop(&mut self) {
        if self.ephemeral.load(Ordering::SeqCst) {
            let root = self.root.lock().clone();
            drop(std::fs::remove_dir_all(root));
        }
    }
}

fn decode_component(encoded: &str) -> Result<String> {
    let chars: Vec<char> = encoded.chars().collect();
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < chars.len() {
        match chars.get(index) {
            Some('%') => {
                let hi = chars
                    .get(index + 1)
                    .and_then(|digit| digit.to_digit(16))
                    .ok_or_else(|| Error::Invalid(format!("malformed escape in {encoded:?}")))?;
                let lo = chars
                    .get(index + 2)
                    .and_then(|digit| digit.to_digit(16))
                    .ok_or_else(|| Error::Invalid(format!("malformed escape in {encoded:?}")))?;
                let byte = u8::try_from(hi * 16 + lo)
                    .map_err(|_error| Error::Invalid(format!("malformed byte in {encoded:?}")))?;
                bytes.push(byte);
                index += 3;
            }
            Some(other) => {
                bytes.push(u32::from(*other) as u8);
                index += 1;
            }
            None => break,
        }
    }
    String::from_utf8(bytes).map_err(|_error| Error::Invalid(format!("non-utf8 name {encoded:?}")))
}

fn decode_tenant(encoded: &str) -> Result<String> {
    let rest = encoded
        .strip_prefix('!')
        .ok_or_else(|| Error::Invalid(format!("malformed tenant directory {encoded:?}")))?;
    decode_component(rest)
}

fn dir_bytes(dir: &Path) -> Result<u64> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut total = 0u64;
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

/// Returns whether a directory holds any entry (a missing directory holds none).
fn dir_has_files(dir: &Path) -> Result<bool> {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Percent-encodes every byte outside `[A-Za-z0-9._-]` so that any UTF-8 name
/// maps to one safe path component.
fn encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if is_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
    }
    out
}

/// Encodes a volume name into a single filesystem-safe component.
fn encode_name(name: &str) -> Result<String> {
    if name.is_empty() {
        return Err(Error::Invalid("volume name must not be empty".to_string()));
    }
    let encoded = encode_component(name);
    if encoded == "." || encoded == ".." {
        return Err(Error::Invalid(format!("unsafe volume name {name:?}")));
    }
    Ok(encoded)
}

/// Encodes a tenant into a directory component.
///
/// Every tenant directory carries a `!` marker prefix so the platform tenant
/// (no name) is distinct from a tenant literally named like its directory: a
/// raw `!` never appears in an encoded component, so the prefix round-trips.
fn encode_tenant(tenant: Option<&str>) -> Result<String> {
    let raw = tenant.unwrap_or("");
    if raw.contains('/') || raw.contains('\\') || raw.contains('\0') {
        return Err(Error::Invalid(format!("unsafe tenant name {raw:?}")));
    }
    Ok(format!("!{}", encode_component(raw)))
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn unique_temp_root() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "selium-volumes-{}-{nanos}-{unique}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry() -> VolumeRegistry {
        VolumeRegistry::ephemeral()
    }

    /// Two independently opened handles share one append domain: every
    /// returned range is unique and complete payloads read back in offset
    /// order.
    // scenario: selium-kernel/concurrent-volume-appends-have-unique-ordered-offsets/two-handles-append-concurrently
    #[test]
    fn concurrent_handles_append_unique_ordered_ranges() {
        let registry = temp_registry();
        let first = registry.open(Some("acme"), "orders").expect("open first");
        let second = registry.open(Some("acme"), "orders").expect("open second");

        let mut workers = Vec::new();
        for (local_id, payload) in [
            (first.local_id, b"first-payload".to_vec()),
            (second.local_id, b"second-payload".to_vec()),
        ] {
            for _ in 0..50 {
                let registry = registry.clone();
                let payload = payload.clone();
                workers.push(std::thread::spawn(move || {
                    let offset = registry.append(local_id, &payload).expect("append");
                    (offset, payload)
                }));
            }
        }

        let mut written: Vec<(u64, Vec<u8>)> = workers
            .into_iter()
            .map(|worker| worker.join().expect("append worker"))
            .collect();
        written.sort_by_key(|(offset, _)| *offset);

        let mut expected_offset = 0u64;
        for (offset, payload) in &written {
            assert_eq!(
                *offset, expected_offset,
                "appends must be contiguous and non-overlapping"
            );
            assert_eq!(
                registry
                    .read_at(first.local_id, *offset, payload.len())
                    .expect("read"),
                *payload,
                "each returned range must hold its complete payload"
            );
            expected_offset += payload.len() as u64;
        }
        assert_eq!(registry.len(first.local_id).expect("len"), expected_offset);
    }

    /// A write that cannot complete is reported, and the failed append does
    /// not advance the volume: recovery never sees a half-written payload as
    /// a committed append.
    // scenario: selium-kernel/concurrent-volume-appends-have-unique-ordered-offsets/partial-append-failure
    #[test]
    fn an_append_that_cannot_be_written_is_reported_without_advancing() {
        let registry = temp_registry();
        let volume = registry.open(Some("acme"), "orders").expect("open");
        let before = registry.len(volume.local_id).expect("len");

        // Swap the handle's descriptor for a read-only one, so the write
        // itself fails after the file is open (exactly the partial-write I/O
        // failure the kernel must report).
        let path = registry
            .root_path()
            .join(VOLUMES_DIR)
            .join(encode_tenant(Some("acme")).expect("tenant component"))
            .join(encode_name("orders").expect("name component"));
        let read_only = OpenOptions::new()
            .read(true)
            .open(&path)
            .expect("reopen read-only");
        let handle = registry.handle(volume.local_id).expect("handle");
        *handle.file.lock() = read_only;

        assert!(
            matches!(
                registry.append(volume.local_id, b"payload"),
                Err(Error::Io(_))
            ),
            "a failed write is reported, not acknowledged"
        );
        assert_eq!(
            registry.len(volume.local_id).expect("len"),
            before,
            "a failed append leaves the volume at its previous length"
        );
    }

    /// Closing every handle releases the volume's append lock, so opening
    /// transient volumes does not grow host state for the process lifetime.
    #[test]
    fn closed_handles_release_their_append_locks() {
        let registry = temp_registry();
        for index in 0..4 {
            let volume = registry
                .open(Some("acme"), &format!("transient-{index}"))
                .expect("open");
            registry.close(volume.local_id).expect("close");
        }
        let kept = registry.open(Some("acme"), "kept").expect("open");
        assert_eq!(
            registry.inner.append_locks.lock().len(),
            1,
            "only the live volume keeps an append domain"
        );
        registry.close(kept.local_id).expect("close");
    }

    // scenario: selium-kernel/durable-volume-primitives/contents-are-opaque
    #[test]
    fn append_and_read_round_trip() {
        let registry = temp_registry();
        let volume = registry.open(Some("acme"), "orders").expect("open");
        assert_eq!(volume.len, 0);
        let offset = registry.append(volume.local_id, b"hello").expect("append");
        assert_eq!(offset, 0);
        let offset = registry.append(volume.local_id, b" world").expect("append");
        assert_eq!(offset, 5);
        assert_eq!(registry.len(volume.local_id).expect("len"), 11);
        assert_eq!(
            registry.read_at(volume.local_id, 6, 100).expect("read"),
            b"world".to_vec()
        );
        assert_eq!(
            registry.read_at(volume.local_id, 0, 5).expect("read"),
            b"hello".to_vec()
        );
        assert_eq!(
            registry.read_at(volume.local_id, 64, 8).expect("read"),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn list_and_remove_cover_the_volume_lifecycle() {
        let registry = temp_registry();
        for name in ["orders", "with/slash", "utf8-\u{00e9}"] {
            let volume = registry.open(Some("acme"), name).expect("open");
            registry.append(volume.local_id, b"bytes").expect("append");
        }
        let names = registry.list(Some("acme")).expect("list");
        assert_eq!(names, vec!["orders", "utf8-\u{00e9}", "with/slash"]);

        let freed = registry.remove(Some("acme"), "orders").expect("remove");
        assert_eq!(freed, 5);
        assert_eq!(
            registry.list(Some("acme")).expect("list"),
            vec!["utf8-\u{00e9}", "with/slash"]
        );
        assert!(matches!(
            registry.remove(Some("acme"), "missing"),
            Err(Error::NotFound(_))
        ));
    }
    // scenario: selium-abi/volume-hostcall-variants/sync-is-a-durable-barrier
    // scenario: selium-abi/tenant-volume-usage-hostcall/reading-reflects-resident-bytes

    // scenario: selium-kernel/durable-volume-primitives/bytes-survive-a-kernel-restart
    #[test]
    fn a_fresh_registry_over_the_same_directory_observes_earlier_bytes() {
        let root = unique_temp_root();
        {
            let registry = VolumeRegistry::new(&root);
            let volume = registry.open(Some("acme"), "orders").expect("open");
            registry
                .append(volume.local_id, b"durable")
                .expect("append");
            registry.sync(volume.local_id).expect("sync");
            assert_eq!(registry.tenant_usage(Some("acme")).expect("usage"), 7);
            assert_eq!(
                registry.usage_by_tenant().expect("usage by tenant"),
                vec![("acme".to_string(), 7)]
            );
        }
        let reopened = VolumeRegistry::new(&root);
        let volume = reopened.open(Some("acme"), "orders").expect("reopen");
        assert_eq!(volume.len, 7);
        assert_eq!(
            reopened.read_at(volume.local_id, 0, 64).expect("read"),
            b"durable".to_vec()
        );
        drop(std::fs::remove_dir_all(&root));
    }

    #[test]
    fn tenants_are_isolated_and_close_releases_the_handle() {
        let registry = temp_registry();
        let acme = registry.open(Some("acme"), "orders").expect("open");
        let beta = registry.open(Some("beta"), "orders").expect("open");
        registry.append(acme.local_id, b"acme").expect("append");
        assert_eq!(registry.len(beta.local_id).expect("len"), 0);
        assert_eq!(registry.tenant_usage(Some("beta")).expect("usage"), 0);

        registry.close(acme.local_id).expect("close");
        assert!(matches!(
            registry.len(acme.local_id),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn platform_tenant_and_named_tenant_are_distinct_directories() {
        let registry = temp_registry();
        let platform = registry.open(None, "orders").expect("open");
        registry.append(platform.local_id, b"root").expect("append");
        assert_eq!(registry.tenant_usage(Some("root")).expect("usage"), 0);
        assert_eq!(registry.tenant_usage(None).expect("usage"), 4);
    }

    #[test]
    fn setting_the_root_is_legal_before_first_use() {
        let registry = VolumeRegistry::ephemeral();
        let root = unique_temp_root();
        registry.set_root(&root).expect("set root");
        assert_eq!(registry.root(), root);
        // The durable layer now lives at the configured root, not the
        // throwaway one.
        let volume = registry.open(Some("acme"), "orders").expect("open");
        registry.append(volume.local_id, b"bytes").expect("append");
        assert!(root.join(VOLUMES_DIR).exists());
        drop(std::fs::remove_dir_all(&root));
    }

    #[test]
    fn setting_the_root_after_use_is_rejected() {
        let root = unique_temp_root();
        let registry = VolumeRegistry::new(unique_temp_root());
        let volume = registry.open(Some("acme"), "orders").expect("open");
        // An open handle is already use, even before any byte is written.
        assert!(matches!(registry.set_root(&root), Err(Error::Invalid(_))));
        registry.close(volume.local_id).expect("close");
        // A created volume is use even once the handle is closed.
        assert!(matches!(registry.set_root(&root), Err(Error::Invalid(_))));
    }

    #[test]
    fn setting_the_root_to_its_current_path_is_a_noop() {
        let root = unique_temp_root();
        let registry = VolumeRegistry::new(&root);
        let volume = registry.open(Some("acme"), "orders").expect("open");
        registry.append(volume.local_id, b"bytes").expect("append");
        registry.set_root(&root).expect("same root is a no-op");
        assert_eq!(
            registry.read_at(volume.local_id, 0, 16).expect("read"),
            b"bytes".to_vec()
        );
        drop(std::fs::remove_dir_all(&root));
    }

    #[test]
    fn unsafe_names_are_rejected() {
        let registry = temp_registry();
        assert!(matches!(registry.open(None, ""), Err(Error::Invalid(_))));
        assert!(matches!(registry.open(None, ".."), Err(Error::Invalid(_))));
        assert!(matches!(
            registry.open(Some("../escape"), "orders"),
            Err(Error::Invalid(_))
        ));
    }
}

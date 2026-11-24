use std::{future::Future, pin::Pin, task::Poll};

use selium_abi::{
    CompletionState, HostcallEnvelope, HostcallOutput, HostcallRequest, OperationId, TaskId,
    decode_rkyv, encode_hostcall_request, unpack_hostcall_status,
};

use crate::{
    GuestError, Result,
    async_runtime::task_id_from_waker,
    error::abi_error_to_guest_error,
    platform::{selium_hostcall_create, selium_hostcall_drop, selium_hostcall_poll},
};

pub(crate) struct HostcallFuture {
    request: Option<HostcallRequest>,
    operation_id: Option<OperationId>,
}

impl Future for HostcallFuture {
    type Output = Result<HostcallOutput>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if self.operation_id.is_none() {
            let Some(task_id) = task_id_from_waker(cx.waker()) else {
                return Poll::Ready(Err(GuestError::Host(
                    "async hostcall polled outside Selium guest reactor".to_string(),
                )));
            };
            let request = self
                .request
                .take()
                .ok_or_else(|| GuestError::Host("hostcall request already consumed".to_string()))?;
            let envelope = HostcallEnvelope {
                request,
                task_id: Some(task_id),
            };
            let encoded = match encode_hostcall_request(&envelope) {
                Ok(encoded) => encoded,
                Err(error) => return Poll::Ready(Err(error.into())),
            };
            // SAFETY: `encoded` is a valid byte buffer; the host validates the request.
            let operation_id = unsafe { selium_hostcall_create(encoded.as_ptr(), encoded.len()) };
            if operation_id == 0 {
                return Poll::Ready(Err(create_failure()));
            }
            self.operation_id = Some(operation_id);
        }

        let operation_id = self.operation_id.expect("operation id set above");
        match poll_operation(operation_id) {
            Ok(Some(output)) => {
                // SAFETY: `operation_id` is valid and was returned by `selium_hostcall_create`.
                unsafe { selium_hostcall_drop(operation_id) };
                self.operation_id = None;
                Poll::Ready(Ok(output))
            }
            Ok(None) => Poll::Pending,
            Err(error) => {
                // SAFETY: `operation_id` is valid and was returned by `selium_hostcall_create`.
                unsafe { selium_hostcall_drop(operation_id) };
                self.operation_id = None;
                Poll::Ready(Err(error))
            }
        }
    }
}

impl Drop for HostcallFuture {
    fn drop(&mut self) {
        if let Some(operation_id) = self.operation_id {
            // SAFETY: `operation_id` was returned by `selium_hostcall_create` and is still valid.
            unsafe { selium_hostcall_drop(operation_id) };
        }
    }
}

/// Authorises `client_process_id` to attach the shared resource named by
/// `resource_class`/`resource_id`. The caller must own the resource or hold
/// an `ExplicitResource` capability for it; no guest identity is privileged.
/// The host records the handoff as the client's attach basis.
pub fn authorise_attach(
    resource_class: selium_abi::ResourceClass,
    resource_id: selium_abi::SharedResourceId,
    client_process_id: selium_abi::ProcessId,
) -> Result<()> {
    hostcall_ready(HostcallRequest::AuthoriseAttach {
        resource_class,
        resource_id,
        client_process_id,
    })
    .map(|_| ())
}

/// Declares a resource the caller owns attachable by resolution, with a
/// declaration scope naming which callers the declaration admits. While a
/// live declaration holds, the host accepts a resolve-recording within its
/// scope as the client's attach basis.
pub fn declare_attachable_by_resolution(
    resource_class: selium_abi::ResourceClass,
    resource_id: selium_abi::SharedResourceId,
    scope: selium_abi::AttachDeclarationScope,
) -> Result<()> {
    hostcall_ready(HostcallRequest::DeclareAttachableByResolution {
        resource_class,
        resource_id,
        scope,
    })
    .map(|_| ())
}

/// The exact reserved URIs the host expects this process to publish.
pub fn expected_claims(process_id: selium_abi::ProcessId) -> Result<Vec<String>> {
    match hostcall_ready(HostcallRequest::ExpectedClaims { process_id })? {
        HostcallOutput::Claims(claims) => Ok(claims),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for ExpectedClaims: {other:?}"
        ))),
    }
}

/// Maps a process tenant to its namespace: `None` (or empty) → `Root`, a
/// named tenant → `Tenant(t)`.
pub fn namespace_from_tenant(tenant: Option<&str>) -> selium_abi::Namespace {
    match tenant {
        Some(tenant) if !tenant.is_empty() => selium_abi::Namespace::Tenant(tenant.to_string()),
        _ => selium_abi::Namespace::Root,
    }
}

/// Returns whether `process_id` holds `capability`.
///
/// Restricted to the discovery system guest: the runtime accepts this
/// hostcall only from the process booted under the `"discovery"` name, so the
/// discovery service can gate root-registration requests against the caller's
/// grants without any other guest probing grant state.
pub fn process_capability(
    process_id: selium_abi::ProcessId,
    capability: selium_abi::Capability,
) -> Result<bool> {
    process_capability_for_uri(process_id, capability, None)
}

/// Returns whether `process_id` holds `capability` in the scope of `uri`.
///
/// A `RouteClaim` names its reserved URI through `ResourceSelector::Uri`, so
/// the probe must evaluate the grant in that URI's scope; the runtime includes
/// `uri` in the `ScopeContext`.
pub fn process_capability_for_uri(
    process_id: selium_abi::ProcessId,
    capability: selium_abi::Capability,
    uri: Option<String>,
) -> Result<bool> {
    match hostcall_ready(HostcallRequest::ProcessCapability {
        process_id,
        capability,
        uri,
    })? {
        HostcallOutput::U64(value) => Ok(value != 0),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for ProcessCapability: {other:?}"
        ))),
    }
}

/// The live processes the host retains authority for.
///
/// Gated by `ProcessProbe`; used by a registry to reconcile expected claims
/// against published registrations.
pub fn process_list() -> Result<Vec<selium_abi::ProcessId>> {
    match hostcall_ready(HostcallRequest::ProcessList)? {
        HostcallOutput::Processes(processes) => Ok(processes),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for ProcessList: {other:?}"
        ))),
    }
}

/// Derives a caller's requestor namespace from its process owner.
///
/// The tenant is read from the runtime's persisted process authority — the
/// only non-forgeable tenant source — never from a tenant the caller asserts
/// for itself (e.g. sender-controlled handoff metadata). A named process
/// tenant yields `Tenant(t)`; an unset (root) tenant yields `Root`. Returns an
/// error only when the runtime tenant lookup itself fails.
pub fn process_namespace(process_id: selium_abi::ProcessId) -> Result<selium_abi::Namespace> {
    process_tenant(process_id).map(|tenant| namespace_from_tenant(tenant.as_deref()))
}

/// The tenant identity assigned to another process, if any.
///
/// The discovery service uses this to scope resolution to the calling
/// process's own tenant.
pub fn process_tenant(process_id: selium_abi::ProcessId) -> Result<Option<String>> {
    match hostcall_ready(HostcallRequest::ProcessTenant { process_id })? {
        HostcallOutput::Tenant(tenant) => Ok(tenant),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for ProcessTenant: {other:?}"
        ))),
    }
}

/// Removes a host-held quota counter for a tenant and resource class,
/// restoring unrestricted allocation. Requires the `QuotaWrite` capability.
pub fn quota_clear(tenant: &str, class: selium_abi::ResourceClass) -> Result<()> {
    hostcall_ready(HostcallRequest::QuotaClear {
        tenant: tenant.to_string(),
        class,
    })
    .map(|_| ())
}

/// Stores a host-held quota counter for a tenant and resource class. Requires
/// the bootstrap-provisioned, non-conferable `QuotaWrite` capability, held
/// solely by the accounting guest.
pub fn quota_set(tenant: &str, class: selium_abi::ResourceClass, limit: u64) -> Result<()> {
    hostcall_ready(HostcallRequest::QuotaSet {
        tenant: tenant.to_string(),
        class,
        limit,
    })
    .map(|_| ())
}

/// Fills a buffer with cryptographically secure random bytes from the host.
///
/// Used by TLS-terminating guests on wasm32 where no OS entropy source is
/// available. The host enforces a maximum length (currently 4096 bytes);
/// legitimate TLS operations request well under that limit.
pub fn random_bytes(len: u32) -> Result<Vec<u8>> {
    match hostcall_ready(HostcallRequest::RandomBytes { len })? {
        HostcallOutput::RandomBytes(bytes) => Ok(bytes),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for RandomBytes: {other:?}"
        ))),
    }
}

/// Records that a resolve performed by `client_process_id` returned the named
/// shared resource. Accepted only under a live attachable-by-resolution
/// declaration whose scope admits the client; the recorded id gives the
/// resolving client an authorisation basis for the corresponding cross-process
/// attach hostcall (`HostQueueAttach` for queues, `AttachRegion` for regions).
pub fn record_resolved_resource_for(
    resource_class: selium_abi::ResourceClass,
    resource_id: selium_abi::SharedResourceId,
    client_process_id: selium_abi::ProcessId,
) -> Result<()> {
    hostcall_ready(HostcallRequest::RecordResolvedResourceFor {
        resource_class,
        resource_id,
        client_process_id,
    })
    .map(|_| ())
}

/// The authoritative ownership answer for `(class, resource_id)`.
///
/// Answers from the host's mint-time records, which are updated synchronously
/// as the host mints a resource. A guest cannot reconstruct that reliably from
/// a pushed feed — the feed can lag a resource the caller has only just
/// created — so this is the question to ask before admitting anything that
/// depends on ownership.
pub fn resource_authority(
    process_id: selium_abi::ProcessId,
    class: selium_abi::ResourceClass,
    resource_id: u64,
) -> Result<selium_abi::ResourceAuthority> {
    match hostcall_ready(HostcallRequest::ProcessOwnsResource {
        process_id,
        resource_class: class,
        resource_id,
    })? {
        HostcallOutput::ResourceAuthority(authority) => Ok(authority),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for ProcessOwnsResource: {other:?}"
        ))),
    }
}

/// Resolves the current discovery listener after a discovery session failure.
///
/// This is the bootstrap complement to discovery's published route: a dead
/// discovery service cannot resolve its own replacement. The host returns the
/// current queue only after its owner declared it attachable by resolution and
/// records that resolution as this process's attach basis.
pub fn discovery_listener() -> Result<selium_abi::SharedResourceId> {
    match hostcall_ready(HostcallRequest::DiscoveryListener)? {
        HostcallOutput::SharedId(shared_id) => Ok(shared_id),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for DiscoveryListener: {other:?}"
        ))),
    }
}

/// Deletes a tenant CA key from the host keyring. Requires the
/// `MintCertificate` capability.
pub fn revoke_ca(tenant: &str) -> Result<()> {
    hostcall_ready(HostcallRequest::RevokeCa {
        tenant: tenant.to_string(),
    })
    .map(|_| ())
}

/// The calling process's own identity: process id and tenant scope.
///
/// System guests use this to verify handoff identities against their own
/// tenant (e.g. the bridge-server refuses clients whose authenticated
/// tenant scope differs from its own).
pub fn self_info() -> Result<(selium_abi::ProcessId, Option<String>)> {
    match hostcall_ready(HostcallRequest::SelfInfo)? {
        HostcallOutput::SelfInfo { process_id, tenant } => Ok((process_id, tenant)),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for SelfInfo: {other:?}"
        ))),
    }
}

/// Mints a tenant CA through the host signing oracle, returning the
/// DER-encoded tenant CA certificate. Requires the `MintCertificate`
/// capability; the tenant CA private key never reaches the guest.
pub fn sign_tenant_ca(tenant: &str) -> Result<Vec<u8>> {
    match hostcall_ready(HostcallRequest::SignTenantCa {
        tenant: tenant.to_string(),
    })? {
        HostcallOutput::Certificate(der) => Ok(der),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for SignTenantCa: {other:?}"
        ))),
    }
}

/// Discards the host-private CA rollback snapshot after the tenant registry
/// record is durable. Requires the `MintCertificate` capability.
pub fn commit_tenant_ca(tenant: &str, ca_cert_der: &[u8]) -> Result<()> {
    hostcall_ready(HostcallRequest::CommitTenantCa {
        tenant: tenant.to_string(),
        ca_cert_der: ca_cert_der.to_vec(),
    })
    .map(|_| ())
}

/// Restores the CA displaced by a mint whose registry append definitely
/// failed. Requires the `MintCertificate` capability.
pub fn rollback_tenant_ca(tenant: &str, ca_cert_der: &[u8]) -> Result<()> {
    hostcall_ready(HostcallRequest::RollbackTenantCa {
        tenant: tenant.to_string(),
        ca_cert_der: ca_cert_der.to_vec(),
    })
    .map(|_| ())
}

/// Signs a client-supplied SPKI into a short-TTL user leaf certificate,
/// returning the DER-encoded leaf. Requires the `MintCertificate` capability.
pub fn sign_user_cert(tenant: &str, spki_der: &[u8]) -> Result<Vec<u8>> {
    match hostcall_ready(HostcallRequest::SignUserCert {
        tenant: tenant.to_string(),
        spki_der: spki_der.to_vec(),
    })? {
        HostcallOutput::Certificate(der) => Ok(der),
        other => Err(GuestError::Host(format!(
            "unexpected hostcall output for SignUserCert: {other:?}"
        ))),
    }
}

pub(crate) fn hostcall_async(request: HostcallRequest) -> HostcallFuture {
    HostcallFuture {
        request: Some(request),
        operation_id: None,
    }
}

pub(crate) fn hostcall_ready(request: HostcallRequest) -> Result<HostcallOutput> {
    let envelope = HostcallEnvelope {
        request,
        task_id: None,
    };
    let request = encode_hostcall_request(&envelope)?;
    // SAFETY: `request` is a valid byte buffer; the host validates the contents.
    let operation_id = unsafe { selium_hostcall_create(request.as_ptr(), request.len()) };
    if operation_id == 0 {
        return Err(create_failure());
    }

    match poll_operation(operation_id) {
        Ok(Some(output)) => {
            // SAFETY: `operation_id` was returned by `selium_hostcall_create` and is still valid.
            unsafe { selium_hostcall_drop(operation_id) };
            Ok(output)
        }
        Ok(None) => {
            // SAFETY: `operation_id` was returned by `selium_hostcall_create` and is still valid.
            unsafe { selium_hostcall_drop(operation_id) };
            Err(GuestError::Host(
                "hostcall returned pending; await the async API instead".to_string(),
            ))
        }
        Err(error) => {
            // SAFETY: `operation_id` was returned by `selium_hostcall_create` and is still valid.
            unsafe { selium_hostcall_drop(operation_id) };
            Err(error)
        }
    }
}

/// A zero create result means the host exhausted the process's operation
/// budget before it could register an operation. Every nonzero result is an
/// operation whose ready, pending, or failed status is obtained by polling.
fn create_failure() -> GuestError {
    GuestError::Hostcall {
        code: selium_abi::AbiErrorCode::MalformedPayload,
        message: "host rejected the hostcall without a structured reason".to_string(),
    }
}

/// Synchronous hostcall that includes a task_id in the envelope.
/// Used for WaitRegister where the host needs to know which guest
/// task to wake on a generation advance.
pub(crate) fn hostcall_ready_with_task(
    request: HostcallRequest,
    task_id: TaskId,
) -> Result<HostcallOutput> {
    let envelope = HostcallEnvelope {
        request,
        task_id: Some(task_id),
    };
    let encoded = encode_hostcall_request(&envelope)?;
    // SAFETY: `encoded` is a valid byte buffer; the host validates the contents.
    let operation_id = unsafe { selium_hostcall_create(encoded.as_ptr(), encoded.len()) };
    if operation_id == 0 {
        return Err(create_failure());
    }

    match poll_operation(operation_id) {
        Ok(Some(output)) => {
            // SAFETY: `operation_id` was returned by `selium_hostcall_create` and is still valid.
            unsafe { selium_hostcall_drop(operation_id) };
            Ok(output)
        }
        Ok(None) => {
            // SAFETY: `operation_id` was returned by `selium_hostcall_create` and is still valid.
            unsafe { selium_hostcall_drop(operation_id) };
            Err(GuestError::Host(
                "hostcall returned pending; await the async API instead".to_string(),
            ))
        }
        Err(error) => {
            // SAFETY: `operation_id` was returned by `selium_hostcall_create` and is still valid.
            unsafe { selium_hostcall_drop(operation_id) };
            Err(error)
        }
    }
}

pub(crate) fn poll_operation(operation_id: OperationId) -> Result<Option<HostcallOutput>> {
    let mut output = vec![0_u8; 4096];
    loop {
        // SAFETY: `output` is a valid mutable buffer; `operation_id` was returned by
        // `selium_hostcall_create`.
        let poll_status =
            unsafe { selium_hostcall_poll(operation_id, output.as_mut_ptr(), output.len()) };
        let (status, len) = unpack_hostcall_status(poll_status);
        if status == selium_abi::HOSTCALL_STATUS_OUTPUT_TOO_SMALL {
            output.resize(len as usize, 0);
            continue;
        }
        let state: CompletionState = decode_rkyv(
            output
                .get(..len as usize)
                .ok_or_else(|| GuestError::Host("invalid hostcall output length".to_string()))?,
        )?;
        return match state {
            CompletionState::Ready(output) => Ok(Some(output)),
            CompletionState::Pending { .. } => Ok(None),
            CompletionState::Failed(error) => Err(abi_error_to_guest_error(error)),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::Namespace;

    /// A root (unset) process tenant maps to the root namespace.
    #[test]
    fn root_process_tenant_maps_to_root_namespace() {
        assert_eq!(namespace_from_tenant(None), Namespace::Root);
        assert_eq!(namespace_from_tenant(Some("")), Namespace::Root);
    }

    /// A named process tenant maps to its tenant namespace.
    #[test]
    fn named_process_tenant_maps_to_tenant_namespace() {
        assert_eq!(
            namespace_from_tenant(Some("acme")),
            Namespace::Tenant("acme".to_string())
        );
    }

    // scenario: selium-guest/guest-errors-preserve-stable-abi-causes/immediate-hostcall-rejection
    #[test]
    fn a_valid_immediately_failed_operation_is_polled_and_dropped() {
        let operation_id = u64::from(u32::MAX) + 7;
        crate::platform::install_native_hostcall_script(crate::platform::NativeHostcallScript {
            immediate_failure: Some(selium_abi::AbiError::new(
                selium_abi::AbiErrorCode::QuotaExceeded,
                "region quota exhausted",
            )),
            next_operation: Some(operation_id),
            ..crate::platform::NativeHostcallScript::default()
        });
        let result = hostcall_ready(HostcallRequest::AllocRegion {
            pages: 1,
            prot: selium_abi::RegionProt::ReadWrite,
            purpose: selium_abi::ResourceKind::SharedMemory,
            serving_tenant: None,
        });
        let error = result.expect_err("the failed operation must surface its ABI error");
        assert_eq!(
            error.abi_code(),
            Some(selium_abi::AbiErrorCode::QuotaExceeded),
            "unexpected error: {error:?}"
        );
        assert!(matches!(
            &error,
            GuestError::Hostcall { message, .. } if message == "region quota exhausted"
        ));
        assert_eq!(
            crate::platform::native_hostcall_drops(),
            vec![operation_id],
            "the full-width failed operation must be dropped"
        );
        assert_eq!(crate::platform::native_hostcall_live_operations(), 0);
        crate::platform::clear_native_hostcall_script();
    }

    #[test]
    fn synchronous_create_paths_preserve_operation_ids_above_u32() {
        // The high word deliberately equals the legacy packed FAILED status:
        // create no longer has a status word, so this remains a valid id.
        let operation_id = (u64::from(selium_abi::HOSTCALL_STATUS_FAILED) << 32) | 0x1234;
        crate::platform::install_native_hostcall_script(crate::platform::NativeHostcallScript {
            next_operation: Some(operation_id),
            time: std::collections::VecDeque::from([
                selium_abi::CompletionState::Ready(HostcallOutput::U64(42)),
                selium_abi::CompletionState::Ready(HostcallOutput::U64(43)),
            ]),
            ..crate::platform::NativeHostcallScript::default()
        });

        assert_eq!(
            hostcall_ready(HostcallRequest::TimeMonotonic).expect("poll the full-width operation"),
            HostcallOutput::U64(42)
        );
        assert_eq!(
            hostcall_ready_with_task(HostcallRequest::TimeMonotonic, 99)
                .expect("the task-aware path also preserves the id"),
            HostcallOutput::U64(43)
        );
        assert_eq!(
            crate::platform::native_hostcall_drops(),
            vec![operation_id, operation_id + 1]
        );
        assert_eq!(crate::platform::native_hostcall_live_operations(), 0);
        crate::platform::clear_native_hostcall_script();
    }

    #[test]
    fn asynchronous_create_preserves_the_u64_max_operation_id() {
        crate::platform::install_native_hostcall_script(crate::platform::NativeHostcallScript {
            next_operation: Some(u64::MAX),
            time: std::collections::VecDeque::from([selium_abi::CompletionState::Ready(
                HostcallOutput::U64(43),
            )]),
            ..crate::platform::NativeHostcallScript::default()
        });
        let mut future = hostcall_async(HostcallRequest::TimeMonotonic);
        let waker = crate::async_runtime::test_task_waker();
        let mut context = std::task::Context::from_waker(&waker);

        assert!(matches!(
            std::pin::Pin::new(&mut future).poll(&mut context),
            Poll::Ready(Ok(HostcallOutput::U64(43)))
        ));
        assert_eq!(crate::platform::native_hostcall_drops(), vec![u64::MAX]);
        assert_eq!(crate::platform::native_hostcall_live_operations(), 0);
        crate::platform::clear_native_hostcall_script();
    }

    #[test]
    fn a_zero_create_result_reports_operation_exhaustion_without_polling() {
        crate::platform::install_native_hostcall_script(crate::platform::NativeHostcallScript {
            next_operation: Some(0),
            time: std::collections::VecDeque::from([selium_abi::CompletionState::Ready(
                HostcallOutput::U64(44),
            )]),
            ..crate::platform::NativeHostcallScript::default()
        });

        let error = hostcall_ready(HostcallRequest::TimeMonotonic)
            .expect_err("zero is exhaustion, not an operation id");
        assert_eq!(
            error.abi_code(),
            Some(selium_abi::AbiErrorCode::MalformedPayload)
        );
        assert!(matches!(
            error,
            GuestError::Hostcall {
                code: selium_abi::AbiErrorCode::MalformedPayload,
                ..
            }
        ));
        assert!(crate::platform::native_hostcall_drops().is_empty());
        assert_eq!(crate::platform::native_hostcall_live_operations(), 0);
        crate::platform::clear_native_hostcall_script();
    }
}

# Tasks

## 1. Foundation: Namespace and Delegation

- [x] 1.1 Add `Namespace { Root, Tenant(String) }` to `selium-abi` (`crates/abi/src/lib.rs`) with rkyv derive and `FlatMsg` codec compatibility. Verify: `cargo build -p selium-abi` and a unit test that `encode_rkyv`/`decode_rkyv` round-trips both variants.
- [x] 1.2 Add `ResourceSelector::Namespace(Namespace)` and implement `matches`: `Root` matches a `None` tenant, `Tenant(t)` matches `Some(t)` exactly. Verify: selector unit tests in `crates/abi` cover exact-tenant and root admission.
- [x] 1.3 Update `validate_grants` in `crates/runtime/src/process.rs` so a `DelegateGrants` grant is admitted when it carries `Namespace::Root` or a `Tenant`/`Namespace::Tenant` selector, and a selector-less `DelegateGrants` is still rejected. Verify existing `DelegateGrants` validation tests plus a new case admitting `Namespace::Root`.
- [x] 1.4 Update the delegation fence in `crates/runtime/src/hostcall.rs` so `Namespace::Root` in a parent's `DelegateGrants` admits child grants for any tenant, while tenant-scoped delegation stays tenant-fenced. Verify a unit test that a root-scoped delegator spawns a tenant-scoped child it does not itself hold.
- [x] 1.5 Add a process-owner namespace helper in `selium-guest`: `namespace_from_tenant(Option<&str>)` (`None`/empty -> `Root`, named -> `Tenant(t)`) and `process_namespace(pid)` deriving a caller's namespace from `process_tenant`. Verify unit tests for the root and named-tenant mappings.
- [x] 1.6 Gate tenant-scoped spawn on `DelegateGrants` for every parent — a root parent no longer spawns a child under a tenant by accident of its bootstrap tenant. Verify a unit test that a root principal without `DelegateGrants` is denied a tenant-scoped spawn, alongside the root-delegator success case.

## 2. Bridge-Server Single Instance

- [x] 2.1 In `guests/bridge/src/lib.rs`, remove the `self_info()` own-tenant requirement and refusal, and serve the root route (path `["bridge"]` -> `sel:///bridge`). Verify the bridge builds and its doc header/comment updates reflect single-instance serving.
- [x] 2.2 Replace the `identity.tenant != own_tenant` refusal with per-handoff tenant from the decoded identity; thread that tenant through `narrowing_for` and `bridge_channel_grants`. Verify bridge unit tests updated and a new test that two handoffs with different tenants confer child grants scoped to their own tenants.
- [x] 2.3 Update the bridge-server descriptor (`crates/runtime/tests/spine_common/mod.rs` and related builders): `tenant: None`, add `SystemRegistration`, and replace tenant-scoped `DelegateGrants` with `Namespace::Root`. Verify the bridge boots as a root guest in the bridge/bootstrap integration test.

## 3. Control-Plane Single Instance

- [x] 3.1 ~~Pin the control-plane listener to the `sel-quic` connector.~~ *Superseded during implementation: the connector/pinning model was dropped — the control-plane serves internal shared-memory sessions only and derives tenant from the process owner (see 3.3 and the corrected `control-plane` spec).*
- [x] 3.2 In `guests/control-plane/src/lib.rs`, remove the `self_info()` tenant refusal and register the root route (path `["control"]` -> `sel:///control`, wire name `control`). Verify the `control_plane` bootstrap test (`cargo test -p selium-runtime --test control_plane -- --ignored`) asserts the `sel:///control` registration.
- [x] 3.3 Derive the requestor's tenant per session from the delivering process's tenant (its process owner): `process_namespace(client_process_id)` -> `Tenant(t)`/`Root`; refuse the session only when the process-tenant lookup fails. Verify a unit test that a named process tenant scopes the session and a root process tenant scopes root.
- [x] 3.4 Tag each desired-state record with its tenant and partition `ControlPlaneState` by `Namespace`, so `Status`/`Scale`/`Stop` resolve into the session's tenant partition (update `DesiredStateRecord` in `selium-service` if needed). Verify control-plane tests updated plus a new test that two tenants' deployments do not leak across partitions.
- [x] 3.5 Prefix module-blob manifests with the session's tenant (`<tenant>:<manifest>`) in the single control-plane blob store. Verify a unit test that an upload in an `acme` session records an `acme:`-prefixed manifest and deploy references it.
- [x] 3.6 Update the control-plane descriptor and `control_plane_grants`: drop tenant selectors (class-scoped `Storage`/`SharedMemory`/`HostQueue`) and add `SystemRegistration`. Verify `control_plane` tests (`control_plane_grants`, `admit_control_client`) updated for the class-scoped grants.

## 4. Addressing and Integration

- [x] 4.1 Verify root wire-name projection through `selium_abi::uri::resolve_wire_name`: bare `control`/`bridge` project to `sel:///control`/`sel:///bridge` with no tenant suffix. Verify addressing tests cover the bare-name case alongside the existing `bridge.acme` case.
- [x] 4.2 Collapse the per-tenant control-plane/bridge descriptors in the integration-test builders to single descriptors, and add a per-tenant isolation test at the handoff substrate (`bridge_handoff`): two handoffs with different tenants are served by the single root bridge-server, each spawns a bridge-channel scoped to its own identity's tenant, each child attaches only its own handed-off region, and both cross-tenant attaches are denied with `PermissionDenied`. Tenant isolation of the desired-state read model is asserted by the control-plane guest's partition tests (3.4). Verify the substrate test asserts cross-tenant isolation end to end.
- [x] 4.3 Update the golden-path spine and bridge-handoff tests to dial the bare `bridge` name (the control surface is internal, reached through the bridge) and to present a leaf whose tenant drives scope. Verify `cargo test -p selium-runtime --test spine -- --ignored` and the bridge handoff test pass after the wasm32 guest build.
- [x] 4.4 Update the `sel` CLI caller (`crates/cli`): dial the bare `bridge` server name, open the control channel on `sel:///control`, and drop the now-dead `--tenant` flag — the tenant comes from the presented leaf, not the name dialled. Verify the connect-options unit test asserts the bare name and root route.

## 5. Validation

- [x] 5.1 Run the full pre-commit gate: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`, `cargo build --target wasm32-unknown-unknown -p selium-spine-demo -p selium-discovery`, `cargo test -p selium-runtime --test spine -- --ignored`, and `scripts/check-wasm-patches.sh`. Verify all pass.

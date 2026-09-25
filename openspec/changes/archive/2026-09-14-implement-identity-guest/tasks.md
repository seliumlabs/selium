## 1. ABI Surface

- [x] 1.1 Add `Capability::MintCertificate` variant to `selium-abi` with rkyv derives, and verify `cargo test -p selium-abi` round-trip and admission-matrix tests pass
- [x] 1.2 Add `SignTenantCa`, `SignUserCert`, and `RevokeCa` variants to `HostcallRequest` and their DER-certificate-only outputs to `HostcallOutput`, and verify encode/decode round-trip tests pass
- [x] 1.3 Add identity request/response message types (operator/tenant tiers) to `selium-service`, and verify schema generation and codec tests pass

## 2. Capability Enforcement

- [x] 2.1 Implement `MintCertificate` bootstrap-only provisioning and spawn-time non-conferability in the runtime admission matrix, and verify the capability-enforcement scenarios (bootstrap grant + spawn denial) pass

## 3. Runtime Keyring and Signing

- [x] 3.1 Add a host-held keyring (offline root, online intermediate, per-tenant CA keys) with a pluggable backing, and verify unit tests cover insert/lookup/delete and that no read path returns private keys
- [x] 3.2 Implement signing hostcall dispatch — `SignTenantCa` generates a tenant CA keypair and signs via the intermediate, `SignUserCert` signs a client SPKI via the tenant's CA key, `RevokeCa` deletes the tenant key — gated by `MintCertificate`, and verify hostcall tests assert denial without the capability and no-private-key-output
- [x] 3.3 Initialize the keyring at runtime startup (load/unlock the intermediate) and verify startup fails loudly when the intermediate is missing

## 4. Identity Guest

- [x] 4.1 Create the `crates/guests/identity` crate (`selium-identity`) with a zero-argument entrypoint and interface metadata, and verify it compiles and boots as a system guest
- [x] 4.2 Implement the durable tenant registry over `Storage` logs with replay, and verify a restart-replay test restores tenant state
- [x] 4.3 Implement the durable principal registry (`fingerprint -> baseline grants`) with replay, and verify a restart-replay test restores principals and grants
- [x] 4.4 Publish the anchor live table (`client-ca-<tenant>`) and the grant live table (`fp -> grants`), and verify both reflect registry state
- [x] 4.5 Implement the tiered request surface (operator vs tenant tiers) delegating mint operations to the hostcalls, and verify tier-refusal (tenant-tier cannot mint a tenant CA) and mint tests
- [x] 4.6 Implement tenant CA lifecycle — mint, rotate, revoke — including anchor removal and key deletion, and verify the revocation scenario removes the anchor and key

## 5. Connector

- [x] 5.1 Switch `quic-connector` to build its `ClientAnchorSet` from the identity-published anchor live table instead of static `tls-certs` blob manifests, and verify the connector constructs its anchor set from the table
- [x] 5.2 Rebuild the connector's client union verifier on anchor add/remove and refuse to serve before initial anchor state, and verify anchor-propagation and revocation scenarios pass

## 6. Bridge

- [x] 6.1 Replace the bridge-server's interim `IdentityGrantMap` stub with a read of the identity-published grant table, and verify the bridge confers from the table and refuses unknown fingerprints

## 7. Bootstrap and Integration

- [x] 7.1 Wire the identity `SystemGuestDescriptor` (grants including `MintCertificate`; readiness: keyring initialized, registries replayed, tables live) with connector and bridge readiness dependening on identity, and verify the bootstrap-order test passes
- [x] 7.2 Validate the single-host spine end to end — onboard a tenant, mint a user cert, connect a client through the connector, and resolve grants through the bridge — and verify the golden-path integration test passes

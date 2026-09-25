## ADDED Requirements

### Requirement: Certificate Signing Hostcall Variants

`HostcallRequest` SHALL define certificate-signing variants `SignTenantCa` (sign a generated tenant CA keypair via the online intermediate), `SignUserCert` (sign a client-supplied SPKI via that tenant's CA key), and `RevokeCa` (delete a tenant CA key from the keyring). The corresponding `HostcallOutput` variants SHALL carry DER-encoded public certificates only and SHALL NOT carry private-key material.

#### Scenario: SignTenantCa round-trips

- **WHEN** a guest encodes `HostcallRequest::SignTenantCa` and the runtime processes it
- **THEN** the hostcall SHALL complete with a `HostcallOutput` variant carrying the signed tenant CA certificate

#### Scenario: SignUserCert round-trips

- **WHEN** a guest encodes `HostcallRequest::SignUserCert` with a client SPKI and tenant
- **THEN** the hostcall SHALL complete with a `HostcallOutput` variant carrying the signed leaf certificate

#### Scenario: RevokeCa returns no key material

- **WHEN** a guest encodes `HostcallRequest::RevokeCa` for a tenant
- **THEN** the hostcall SHALL complete and the tenant's CA key SHALL be removed from the keyring

### Requirement: MintCertificate Capability Variant

`Capability` SHALL include a `MintCertificate` variant that gates the certificate-signing hostcalls. The variant SHALL be rkyv-encodable like the other capability variants.

#### Scenario: Signing hostcall requires the capability

- **WHEN** a process without a `MintCertificate` grant invokes a certificate-signing hostcall
- **THEN** the runtime SHALL deny the hostcall with a capability error

### Requirement: Resolved-Resource Recording Hostcall Variants

`HostcallRequest` SHALL define recording variants `RecordResolvedQueueFor` and `RecordResolvedRegionFor`, each naming a client process and the resource id a discovery Resolve returned to it. Both SHALL be accepted only from the discovery system guest; any other caller SHALL be denied with a capability error. A recorded id SHALL give the named client process an authorisation basis for the corresponding cross-process attach hostcall (`HostQueueAttach` for queues, `AttachRegion` for shared regions) on a resource it did not allocate — the basis peer guests use to attach a publishing guest's live tables.

#### Scenario: Non-discovery caller is denied

- **WHEN** a process other than the discovery system guest invokes `RecordResolvedQueueFor` or `RecordResolvedRegionFor`
- **THEN** the runtime SHALL deny the hostcall with a capability error

#### Scenario: Recorded id authorises the attach

- **WHEN** the discovery system guest records a region id resolved by a client process
- **THEN** that client process's subsequent `AttachRegion` on the region SHALL be authorised

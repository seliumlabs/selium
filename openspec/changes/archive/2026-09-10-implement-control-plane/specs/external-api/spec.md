## REMOVED Requirements

### Requirement: Text-Protocol Request Parsing
**Reason**: DESIGN-INTENT rejects text protocols for control in favour of typed RPC everywhere. `selium-external-api` is superseded by the `control-plane` capability, which serves typed FlatBuffers request/reply over the bridge.
**Migration**: Send typed `selium-wire::rpc` requests to `control.<tenant>` instead of parsing whitespace-delimited text commands.

### Requirement: Intent Decomposition into Delegated Interactions
**Reason**: Intent decomposition remains, but it moves to the `control-plane` capability, where intent is typed rather than derived from a text grammar.
**Migration**: Re-express decomposition over the control-plane's typed intent message; see the `control-plane` capability.

### Requirement: Delegation Dispatch via RPC
**Reason**: Delegation dispatch is unchanged in spirit but re-owned by the `control-plane` capability; `selium-external-api` no longer exists as a separate guest.
**Migration**: Dispatch delegated interactions from the control-plane guest; see the `control-plane` capability.

### Requirement: Inbound Network Bridge Interface
**Reason**: The raw-TCP ring-buffer inbound bridge is replaced by the QUIC connector + bridge handoff, so an external client is a normal authenticated bridge participant rather than a raw TCP peer.
**Migration**: Connect through `selium-client` over QUIC to `control.<tenant>`; there is no raw TCP text listener.

### Requirement: ApiContext Bootstrap
**Reason**: The dedicated `ApiContext` (pre-connected discovery/scheduler clients plus an inbound bridge handle) is replaced by the standard bootstrap `Context` plus delegation clients the control-plane guest constructs itself.
**Migration**: Bootstrap the control-plane guest from a `SystemGuestDescriptor`; see the `control-plane` capability.

### Requirement: Client Feedback Response
**Reason**: Client-facing feedback moves to the `control-plane` capability's typed response messages; the text-shaped `ClientFeedback` is superseded.
**Migration**: Return typed control-plane responses instead of the text `ClientFeedback`; see the `control-plane` capability.

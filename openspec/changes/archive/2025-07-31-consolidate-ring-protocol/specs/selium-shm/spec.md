# Spec Delta: selium-shm

## MODIFIED Requirements

### Requirement: Layout/Plumbing Separation

`selium-shm` SHALL expose the ring protocol layout (offsets, codec,
reservation, slots) independently of the global region provider, so host
environments (kernel, runtime, tests) can drive the same protocol over
alternative `MappingBackend`s.

#### Scenario: Layout usable without a provider

- **WHEN** host code with its own `MappingBackend` drives the ring
  primitives
- **THEN** it can do so without installing or touching the global
  `RegionProvider`

#### Scenario: Public API stability

- **WHEN** downstream code imports `FrameHeader`, `RingBuf`, `Channel`,
  or `ChannelRegion`
- **THEN** existing import paths keep working (via re-exports) after the
  layout module lands

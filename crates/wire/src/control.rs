//! Typed per-stream control frames shared between the external client and the
//! bridge channel.
//!
//! Layered on the `selium-wire` codec: a FlatBuffers-encoded
//! [`PipeControl`] payload carried as a normal frame with tag 0 before data
//! relay begins. The handshake is deterministic: the bridge replies with
//! exactly one control frame — [`PipeControl::Accepted`] once the channel is
//! resolved and attached, or [`PipeControl::Terminate`] (followed by stream
//! teardown) on refusal. Data frames are relayed verbatim and are never
//! decoded as control frames.
//!
//! The type and its termination terms are owned by `selium-service` (the
//! single authority for service messages); this module re-exports them so
//! `selium-wire` consumers keep their existing imports and framing story.

pub use selium_service::{PipeControl, TERMINATE_ATTACH_FAILED, TERMINATE_BAD_HANDSHAKE};

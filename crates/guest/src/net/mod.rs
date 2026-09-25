//! Guest networking: raw TCP and UDP sockets over shared-memory ring buffers.
//!
//! `TcpStream` and `TcpListener` provide byte-stream TCP, while `UdpSocket`
//! provides datagram UDP with binary-addressed frames. All addresses are
//! IP literals only — name resolution is a capability-gated typed RPC via
//! the DNS connector.

pub use bytes::ByteStream;
pub use resolve::resolve;
pub use tcp::{TcpListener, TcpStream};
pub use udp::{Datagram, UdpSocket};

pub mod bytes;
pub mod http;
pub mod quic;
pub mod resolve;
pub mod tcp;
pub mod udp;

/// Pins a connector-served listener to the bootstrap-registered protocol
/// handler for `scheme`.
///
/// Handoff metadata is sender-controlled and opaque: without a pin, any
/// guest able to attach the queue could forge handoffs (including the
/// authenticated-identity payload consumed by serve-side guests). The pin
/// is resolved from the runtime's Tier-1 handler registry, which guests
/// cannot forge. Fails closed: binding without a registered handler would
/// leave the listener unable to distinguish legitimate deliveries from
/// forgeries, so it is an error.
pub(crate) fn pin_to_scheme_handler(
    listener: &mut crate::ResourceListener,
    scheme: &str,
) -> crate::Result<()> {
    let handler = crate::resolve_protocol_handler(scheme)?.ok_or_else(|| {
        crate::GuestError::Host(format!(
            "no protocol handler registered for scheme `{scheme}`; refusing to serve unpinned handoffs"
        ))
    })?;
    listener.expect_sender(handler);
    Ok(())
}

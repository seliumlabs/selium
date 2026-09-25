//! Per-stream bidirectional relay pumps.
//!
//! Each accepted QUIC bidirectional stream becomes one two-ring byte channel:
//!
//! - **client → guest**: [`pump_client_to_guest`] copies the QUIC `RecvStream`
//!   into the guest's inbound ring. When the client FINs (read returns 0) the
//!   relayed channel's writer is finished; the caller drops the writer half,
//!   which decrements the ring's `writer_count` and surfaces EOF to the guest.
//!   While the ring is full `write_all` parks and the pump stops reading the
//!   wire, so quinn's receive flow control pushes back to the client — no
//!   unbounded connector-side buffering.
//! - **guest → client**: [`pump_guest_to_client`] copies the guest's outbound
//!   ring into the QUIC `SendStream`. Guest close (ring EOF) finishes the
//!   stream; a read/write error resets it. While the client is slow,
//!   `write_all` parks on quinn flow control, which stops this pump from
//!   reading the ring, so the guest's ring writes park (throttling at the
//!   producer, not the edge).

use std::io;

use selium_guest::net::bytes::{ByteStreamReader, ByteStreamWriter};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Relay copy buffer size.
pub const RELAY_BUFFER_SIZE: usize = 16 * 1024;

/// The write side of a wire stream, able to finish (graceful FIN) or reset.
///
/// Implemented for `quinn::SendStream` in production and by test doubles.
pub trait FinishSink: AsyncWrite + Unpin {
    /// Gracefully finish the stream, sending FIN to the peer.
    fn finish(&mut self);
    /// Reset the stream with an application error.
    fn reset(&mut self, code: quinn::VarInt);
}

impl FinishSink for quinn::SendStream {
    fn finish(&mut self) {
        drop(quinn::SendStream::finish(self));
    }

    fn reset(&mut self, code: quinn::VarInt) {
        drop(quinn::SendStream::reset(self, code));
    }
}

/// Copies bytes from `wire_recv` into `guest_writer` until client EOF (FIN).
///
/// Returns the total bytes relayed. EOF/close propagation to the guest is the
/// caller's responsibility: dropping `guest_writer` after this returns
/// decrements the guest ring's writer count and surfaces EOF.
pub async fn pump_client_to_guest<R, W>(mut wire_recv: R, mut guest_writer: W) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut total = 0u64;
    let mut buf = vec![0u8; RELAY_BUFFER_SIZE];
    loop {
        match wire_recv.read(&mut buf).await {
            Ok(0) => {
                selium_guest::info!("quic-connector: client FIN after {total} bytes");
                return Ok(total);
            }
            Ok(n) => {
                #[expect(
                    clippy::indexing_slicing,
                    reason = "n is bounded by read() return value"
                )]
                guest_writer.write_all(&buf[..n]).await?;
                total += n as u64;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Copies bytes from `guest_reader` into `wire_send` until guest close (EOF).
///
/// Guest close finishes the wire stream (FIN); an error resets it. Slow
/// clients park this pump on quinn flow control, which lets ring backpressure
/// park the guest's writes.
pub async fn pump_guest_to_client<R, W>(mut guest_reader: R, mut wire_send: W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: FinishSink,
{
    let mut buf = vec![0u8; RELAY_BUFFER_SIZE];
    loop {
        match guest_reader.read(&mut buf).await {
            Ok(0) => {
                wire_send.finish();
                return Ok(());
            }
            Ok(n) =>
            {
                #[expect(
                    clippy::indexing_slicing,
                    reason = "n is bounded by read() return value"
                )]
                if let Err(e) = wire_send.write_all(&buf[..n]).await {
                    wire_send.reset(0u32.into());
                    return Err(e);
                }
            }
            Err(e) => {
                wire_send.reset(0u32.into());
                return Err(e);
            }
        }
    }
}

/// Relays both directions of one accepted stream concurrently.
///
/// `guest_reader` reads the guest's outbound ring (guest → client) and
/// `guest_writer` writes the guest's inbound ring (client → guest), as built
/// by [`QuicChannel::into_halves`](crate::stream::QuicChannel::into_halves).
pub async fn relay_stream<R, W>(
    wire_recv: R,
    wire_send: W,
    guest_reader: ByteStreamReader,
    guest_writer: ByteStreamWriter,
) where
    R: AsyncRead + Unpin,
    W: FinishSink,
{
    let to_guest = pump_client_to_guest(wire_recv, guest_writer);
    let to_client = pump_guest_to_client(guest_reader, wire_send);
    drop(tokio::join!(to_guest, to_client));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finish-recording test double for the wire send half.
    struct MockSink {
        inner: tokio::io::DuplexStream,
        finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl AsyncWrite for MockSink {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
        }
    }

    impl FinishSink for MockSink {
        fn finish(&mut self) {
            self.finished
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }

        fn reset(&mut self, _code: quinn::VarInt) {}
    }

    #[tokio::test]
    async fn copy_relays_bytes_identical_and_ordered() {
        // Bounded duplex buffers provide backpressure on both sides: writes
        // park when the buffer fills, so a slow consumer must not lose bytes.
        let (mut wire_a, wire_b) = tokio::io::duplex(64);
        let (mut dest_reader, dest_writer) = tokio::io::duplex(64);

        let payload: Vec<u8> = (0..=255u8).cycle().take(64 * 1024).collect();
        let payload_clone = payload.clone();

        // Feed the wire side, then close it (client FIN).
        let feed = tokio::spawn(async move {
            wire_a.write_all(&payload_clone).await.expect("feed write");
            drop(wire_a);
        });

        // Pump the wire into the destination writer.
        let pump = tokio::spawn(async move {
            pump_client_to_guest(wire_b, dest_writer)
                .await
                .expect("pump")
        });

        // Drain the destination and compare.
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = dest_reader.read(&mut buf).await.expect("read");
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }

        let total = pump.await.expect("pump task");
        feed.await.expect("feed task");
        assert_eq!(total, payload.len() as u64);
        assert_eq!(got, payload, "relayed bytes must be identical and ordered");
    }

    #[tokio::test]
    async fn guest_close_finishes_wire_stream() {
        // Guest close (EOF on a channel) must finish the wire send half.
        let (mut guest_reader_probe, mut guest_feed) = tokio::io::duplex(64);
        guest_feed.write_all(b"payload").await.expect("feed");
        drop(guest_feed); // guest closes

        let (mut wire_recv, wire_send) = tokio::io::duplex(64);
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sink = MockSink {
            inner: wire_send,
            finished: finished.clone(),
        };

        pump_guest_to_client(&mut guest_reader_probe, sink)
            .await
            .expect("pump");

        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "guest close must finish the wire stream"
        );

        let mut buf = [0u8; 7];
        wire_recv.read_exact(&mut buf).await.expect("read");
        assert_eq!(&buf, b"payload");
    }
}

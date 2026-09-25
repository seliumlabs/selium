//! UDP datagram socket over shared-memory ring buffers.
//!
//! `UdpSocket` provides `poll_send`/`poll_recv` with binary-addressed frames
//! compatible with the quinn `AsyncUdpSocket` adapter shape.

use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    pin::Pin,
    task::{Context, Poll},
};

use selium_abi::{HostcallOutput, HostcallRequest, RegionProt};
use selium_memory::MultiMemoryHeader;
use selium_shm::{
    Channel, ChannelBackpressure, ChannelRegion,
    channels::{Reader, Writer},
    ring_buf::RingBuf,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{GuestError, Result, hostcall::hostcall_async};

/// UDP frame version byte.
const UDP_FRAME_VERSION: u8 = 1;

/// A datagram carrying a source/destination address and payload.
#[derive(Debug, Clone)]
pub struct Datagram {
    /// Source (on recv) or destination (on send) address.
    pub addr: SocketAddr,
    /// Raw payload bytes.
    pub payload: Vec<u8>,
}

/// A UDP socket backed by shared-memory send/recv rings.
///
/// Datagrams are encoded with the binary frame format
/// (`[ver][family][addr][port][payload]`). The API shape is compatible with
/// a future `quinn` `AsyncUdpSocket` adapter.
pub struct UdpSocket {
    recv_reader: Reader,
    send_writer: Writer,
    /// Keeps the parent shared region alive while the socket is in use.
    _region: selium_memory::Region,
}

impl UdpSocket {
    /// Binds to an IP-literal address and returns a UDP socket.
    ///
    /// The address is validated early for ergonomics; the runtime is the
    /// enforcement point.
    pub async fn bind(addr: &str) -> Result<Self> {
        let _: std::net::SocketAddr = addr
            .parse()
            .map_err(|_e| GuestError::Host(format!("invalid IP literal address: {addr}")))?;

        let output = hostcall_async(HostcallRequest::UdpBind {
            address: addr.to_string(),
        })
        .await?;

        let descriptor = match output {
            HostcallOutput::SharedRegion(d) => d,
            _ => return Err(GuestError::UnexpectedHostcallOutput),
        };

        let shared_id = descriptor.shared_id;

        let region_provider = selium_memory::region_provider()
            .map_err(|e| GuestError::Host(format!("region provider unavailable: {e}")))?;

        let region = region_provider
            .attach(shared_id, None, RegionProt::ReadWrite)
            .map_err(|e| GuestError::Host(format!("attach region failed: {e}")))?;

        let mapping = region.mapping();
        let header = MultiMemoryHeader::parse(mapping.backend(), 0)
            .map_err(|e| GuestError::Host(format!("parse header failed: {e}")))?;

        let recv_entry = header
            .entry(0)
            .map_err(|e| GuestError::Host(format!("recv entry missing: {e}")))?;
        let send_entry = header
            .entry(1)
            .map_err(|e| GuestError::Host(format!("send entry missing: {e}")))?;

        let recv_mapping = mapping
            .sub_region(recv_entry.offset, recv_entry.length)
            .map_err(|e| GuestError::Host(format!("recv sub-region failed: {e}")))?;
        let send_mapping = mapping
            .sub_region(send_entry.offset, send_entry.length)
            .map_err(|e| GuestError::Host(format!("send sub-region failed: {e}")))?;

        let ring_cap = recv_entry
            .length
            .saturating_sub(selium_shm::layout::DATA_OFFSET);

        let recv_region = ChannelRegion::from_mapping_with_id(recv_mapping, ring_cap, shared_id);
        let send_region = ChannelRegion::from_mapping_with_id(send_mapping, ring_cap, shared_id);

        let recv_ring = RingBuf::wrap_region(recv_region)
            .map_err(|e| GuestError::Host(format!("wrap recv ring failed: {e}")))?;
        let send_ring = RingBuf::wrap_region(send_region)
            .map_err(|e| GuestError::Host(format!("wrap send ring failed: {e}")))?;

        let recv_channel = Channel::from_ring(recv_ring, ChannelBackpressure::Park);
        let send_channel = Channel::from_ring(send_ring, ChannelBackpressure::Park);

        let recv_reader = recv_channel.reader();
        let send_writer = send_channel
            .writer()
            .map_err(|e| GuestError::Host(format!("create writer failed: {e}")))?;

        Ok(Self {
            recv_reader,
            send_writer,
            _region: region,
        })
    }

    /// Attempts to send a datagram. Returns `Poll::Pending` if the send ring is full
    /// (after registering a generation wait), or the number of payload bytes sent.
    pub fn poll_send(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        datagram: &Datagram,
    ) -> Poll<Result<usize>> {
        // Encode [FrameHeader][binary datagram frame]
        let frame = encode_udp_frame(datagram.addr, &datagram.payload);
        let payload_len = frame.len() as u32;
        let header = selium_memory::FrameHeader {
            len: payload_len,
            tag: 0,
            flags: 0,
        };
        let header_bytes = header.encode();

        let mut write_buf = Vec::with_capacity(header_bytes.len() + frame.len());
        write_buf.extend_from_slice(&header_bytes);
        write_buf.extend_from_slice(&frame);

        // SAFETY: we project only to the send_writer field; caller upholds pin invariants.
        let this = unsafe { self.get_unchecked_mut() };
        // SAFETY: pin projection through a struct field; caller upholds invariants.
        let writer = unsafe { Pin::new_unchecked(&mut this.send_writer) };
        match writer.poll_write(cx, &write_buf) {
            Poll::Ready(Ok(n)) => {
                let sent = n.saturating_sub(header_bytes.len());
                Poll::Ready(Ok(sent))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(GuestError::Io(e))),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Polls whether the send ring has room for another datagram, without
    /// writing to it.
    ///
    /// This is a non-mutating readiness probe used by the quinn `UdpPoller`
    /// adapter: it checks the shared send ring's free space and, when the ring
    /// is pinned full by a slow reader or writer, parks on the same
    /// generation-wait the [`Writer`] uses under `Park` backpressure, so a
    /// draining peer wakes the probe.
    pub fn poll_send_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        // SAFETY: we project only to the send_writer's region; caller upholds
        // pin invariants.
        let this = unsafe { self.get_unchecked_mut() };
        let region = this.send_writer.region();

        // Minimum writable payload: one 12-byte frame header. If at least that
        // much space is free, a datagram write can proceed.
        let needed = selium_memory::FrameHeader::ENCODED_SIZE as u64;
        match send_ring_writable(region, needed) {
            Ok(true) => Poll::Ready(Ok(())),
            Ok(false) => {
                let generation = region.load_generation().unwrap_or(0);
                if !selium_memory::register_generation_wait(
                    region.region_id(),
                    generation,
                    cx.waker(),
                ) {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    /// Attempts to receive a datagram. Returns `Poll::Pending` if the recv ring is
    /// empty but writers are still connected (after registering a generation wait).
    /// Returns an error if the channel is closed (`writer_count == 0`).
    pub fn poll_recv(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<Datagram>> {
        // Read a frame (header+payload) from the recv ring.
        // Then decode the binary datagram from the payload.
        let mut buf = vec![0u8; 65536];
        let mut read_buf = ReadBuf::new(&mut buf);

        // SAFETY: we project only to the recv_reader field; caller upholds pin invariants.
        let this = unsafe { self.get_unchecked_mut() };
        // SAFETY: pin projection through a struct field; caller upholds invariants.
        let reader = unsafe { Pin::new_unchecked(&mut this.recv_reader) };

        match reader.poll_read(cx, &mut read_buf) {
            Poll::Ready(Ok(())) => {
                let filled = read_buf.filled().len();
                if filled == 0 {
                    return Poll::Ready(Err(GuestError::Host("udp recv ring closed".to_string())));
                }
                let header_size = selium_memory::FrameHeader::ENCODED_SIZE;
                if filled <= header_size {
                    return Poll::Ready(Err(GuestError::Host("short udp recv frame".to_string())));
                }
                let payload = buf.get(header_size..filled).unwrap_or(&[]);
                match decode_udp_frame(payload) {
                    Some(datagram) => Poll::Ready(Ok(datagram)),
                    None => Poll::Ready(Err(GuestError::Host(
                        "malformed udp datagram frame".to_string(),
                    ))),
                }
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(GuestError::Io(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

// SAFETY: Reader and Writer are backed by process-level shared memory mappings.
unsafe impl Send for UdpSocket {}

/// Decode a binary frame into a `Datagram`, returning `None` if malformed.
pub fn decode_datagram(frame: &[u8]) -> Option<Datagram> {
    decode_udp_frame(frame)
}

/// Encode a `Datagram` into the binary frame format.
///
/// Format: `[ver u8][family u8: 4|6][addr 4|16 bytes][port u16 LE][payload…]`
pub fn encode_datagram(datagram: &Datagram) -> Vec<u8> {
    encode_udp_frame(datagram.addr, &datagram.payload)
}

fn addr_bytes_len(addr: &SocketAddr) -> usize {
    match addr {
        SocketAddr::V4(_) => 4,
        SocketAddr::V6(_) => 16,
    }
}

fn decode_udp_frame(frame: &[u8]) -> Option<Datagram> {
    if frame.len() < 8 {
        return None;
    }
    let ver = *frame.first()?;
    if ver != UDP_FRAME_VERSION {
        return None;
    }
    let family = *frame.get(1)?;
    match family {
        4 => {
            if frame.len() < 8 {
                return None;
            }
            let ip = Ipv4Addr::new(
                *frame.get(2)?,
                *frame.get(3)?,
                *frame.get(4)?,
                *frame.get(5)?,
            );
            let port = u16::from_le_bytes([*frame.get(6)?, *frame.get(7)?]);
            let addr = SocketAddr::V4(SocketAddrV4::new(ip, port));
            let payload = frame.get(8..).unwrap_or(&[]).to_vec();
            Some(Datagram { addr, payload })
        }
        6 => {
            if frame.len() < 20 {
                return None;
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(frame.get(2..18)?);
            let ip = Ipv6Addr::from(octets);
            let port = u16::from_le_bytes([*frame.get(18)?, *frame.get(19)?]);
            let addr = SocketAddr::V6(SocketAddrV6::new(ip, port, 0, 0));
            let payload = frame.get(20..).unwrap_or(&[]).to_vec();
            Some(Datagram { addr, payload })
        }
        _ => None,
    }
}

fn encode_udp_frame(addr: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let addr_len = addr_bytes_len(&addr);
    // ver(1) + family(1) + addr + port(2)
    let header_len = 2 + addr_len + 2;
    let mut frame = Vec::with_capacity(header_len + payload.len());
    frame.push(UDP_FRAME_VERSION);
    match addr {
        SocketAddr::V4(v4) => {
            frame.push(4u8);
            frame.extend_from_slice(&v4.ip().octets());
            frame.extend_from_slice(&v4.port().to_le_bytes());
        }
        SocketAddr::V6(v6) => {
            frame.push(6u8);
            frame.extend_from_slice(&v6.ip().octets());
            frame.extend_from_slice(&v6.port().to_le_bytes());
        }
    }
    frame.extend_from_slice(payload);
    frame
}

/// Returns whether `needed` bytes can be written to the send ring without
/// overwriting the slowest blocking reader or writer.
fn send_ring_writable(region: &selium_shm::ChannelRegion, needed: u64) -> Result<bool> {
    let tail = region
        .read_next_tail()
        .map_err(|e| GuestError::Host(e.to_string()))?;
    let reader = region
        .minimum_reader_position()
        .map_err(|e| GuestError::Host(e.to_string()))?
        .unwrap_or(tail);
    let writer = region
        .minimum_writer_position()
        .map_err(|e| GuestError::Host(e.to_string()))?
        .unwrap_or(tail);
    let pinned = reader.min(writer);

    // Available space = capacity - (tail - pinned). A nil pin (no blocking
    // reader/writer registered) leaves the full capacity available.
    let available = pinned
        .saturating_sub(tail)
        .saturating_add(region.capacity());
    Ok(available >= needed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[test]
    fn encode_decode_ipv4_round_trip() {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 7), 5353));
        let payload = b"hello udp".to_vec();
        let frame = encode_udp_frame(addr, &payload);

        // Check frame layout
        assert_eq!(frame[0], 1); // version
        assert_eq!(frame[1], 4); // family = IPv4
        assert_eq!(&frame[2..6], &[203, 0, 113, 7]); // address
        assert_eq!(&frame[6..8], &5353u16.to_le_bytes()); // port
        assert_eq!(&frame[8..], b"hello udp"); // payload

        let decoded = decode_udp_frame(&frame).expect("decode");
        assert_eq!(decoded.addr, addr);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn encode_decode_ipv6_round_trip() {
        let ip = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
        let addr = SocketAddr::V6(SocketAddrV6::new(ip, 443, 0, 0));
        let payload = b"ipv6 datagram".to_vec();
        let frame = encode_udp_frame(addr, &payload);

        assert_eq!(frame[0], 1); // version
        assert_eq!(frame[1], 6); // family = IPv6
        assert_eq!(&frame[2..18], &ip.octets());
        assert_eq!(&frame[18..20], &443u16.to_le_bytes());
        assert_eq!(&frame[20..], b"ipv6 datagram");

        let decoded = decode_udp_frame(&frame).expect("decode");
        assert_eq!(decoded.addr, addr);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn decode_rejects_wrong_version() {
        let frame = [2u8, 4, 127, 0, 0, 1, 0, 80];
        assert!(decode_udp_frame(&frame).is_none());
    }

    #[test]
    fn decode_rejects_unknown_family() {
        let frame = [1u8, 9, 127, 0, 0, 1, 0, 80];
        assert!(decode_udp_frame(&frame).is_none());
    }

    #[test]
    fn decode_rejects_short_frame() {
        assert!(decode_udp_frame(&[1, 4, 127, 0]).is_none());
    }
}

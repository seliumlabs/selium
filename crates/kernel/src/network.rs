use std::{collections::HashMap, net::TcpListener, sync::Arc, sync::atomic::AtomicBool};

use parking_lot::Mutex;
use selium_abi::SharedResourceId;

#[derive(Clone)]
pub struct NetworkState {
    pub(crate) inner: Arc<NetworkStateInner>,
}

pub struct TcpListenerState {
    pub shared_id: SharedResourceId,
    pub running: Arc<AtomicBool>,
    pub _listener: TcpListener,
}

pub struct TcpStreamState {
    pub running: Arc<AtomicBool>,
}

pub struct UdpSocketState {
    pub running: Arc<AtomicBool>,
}

pub(crate) struct NetworkStateInner {
    pub(crate) tcp_listeners: Mutex<HashMap<u64, TcpListenerState>>,
    pub(crate) tcp_streams: Mutex<HashMap<SharedResourceId, TcpStreamState>>,
    pub(crate) udp_sockets: Mutex<HashMap<SharedResourceId, UdpSocketState>>,
}

impl NetworkState {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(NetworkStateInner {
                tcp_listeners: Mutex::new(HashMap::new()),
                tcp_streams: Mutex::new(HashMap::new()),
                udp_sockets: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn insert_tcp_listener(&self, local_id: u64, state: TcpListenerState) {
        self.inner.tcp_listeners.lock().insert(local_id, state);
    }

    pub fn insert_tcp_stream(&self, shared_id: SharedResourceId, state: TcpStreamState) {
        self.inner.tcp_streams.lock().insert(shared_id, state);
    }

    pub fn insert_udp_socket(&self, shared_id: SharedResourceId, state: UdpSocketState) {
        self.inner.udp_sockets.lock().insert(shared_id, state);
    }

    pub fn remove_tcp_listener(&self, local_id: u64) -> Option<TcpListenerState> {
        self.inner.tcp_listeners.lock().remove(&local_id)
    }

    pub fn remove_tcp_stream(&self, shared_id: u64) -> Option<TcpStreamState> {
        self.inner.tcp_streams.lock().remove(&shared_id)
    }

    pub fn remove_udp_socket(&self, shared_id: u64) -> Option<UdpSocketState> {
        self.inner.udp_sockets.lock().remove(&shared_id)
    }

    /// Returns the local addresses of all active TCP listeners.
    pub fn tcp_listener_addrs(&self) -> Vec<std::net::SocketAddr> {
        self.inner
            .tcp_listeners
            .lock()
            .values()
            .filter_map(|state| state._listener.local_addr().ok())
            .collect()
    }

    /// Returns the running flag for a TCP stream, if registered.
    pub fn tcp_stream_running(&self, shared_id: u64) -> Option<Arc<AtomicBool>> {
        self.inner
            .tcp_streams
            .lock()
            .get(&shared_id)
            .map(|s| s.running.clone())
    }

    /// Returns the running flag for a UDP socket, if registered.
    pub fn udp_socket_running(&self, shared_id: u64) -> Option<Arc<AtomicBool>> {
        self.inner
            .udp_sockets
            .lock()
            .get(&shared_id)
            .map(|s| s.running.clone())
    }
}

/// Decodes a binary datagram frame into `(SocketAddr, payload_bytes)`.
/// Returns `None` if the frame is malformed.
pub fn decode_udp_frame(frame: &[u8]) -> Option<(std::net::SocketAddr, &[u8])> {
    if frame.len() < 8 {
        return None;
    }
    if *frame.first()? != 1 {
        return None;
    }
    let family = *frame.get(1)?;
    match family {
        4 => {
            if frame.len() < 8 {
                return None;
            }
            let ip = std::net::Ipv4Addr::new(
                *frame.get(2)?,
                *frame.get(3)?,
                *frame.get(4)?,
                *frame.get(5)?,
            );
            let port = u16::from_le_bytes([*frame.get(6)?, *frame.get(7)?]);
            let addr = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(ip, port));
            Some((addr, frame.get(8..)?))
        }
        6 => {
            if frame.len() < 20 {
                return None;
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(frame.get(2..18)?);
            let ip = std::net::Ipv6Addr::from(octets);
            let port = u16::from_le_bytes([*frame.get(18)?, *frame.get(19)?]);
            let addr = std::net::SocketAddr::V6(std::net::SocketAddrV6::new(ip, port, 0, 0));
            Some((addr, frame.get(20..)?))
        }
        _ => None,
    }
}

/// Encodes a `SocketAddr` + payload into the binary datagram frame format:
/// `[ver u8 = 1][family u8: 4|6][addr 4|16 bytes][port u16 LE][payload…]`
pub fn encode_udp_frame(addr: std::net::SocketAddr, payload: &[u8]) -> Vec<u8> {
    let addr_len = match addr {
        std::net::SocketAddr::V4(_) => 4usize,
        std::net::SocketAddr::V6(_) => 16usize,
    };
    let header_len = 2 + addr_len + 2;
    let mut frame = Vec::with_capacity(header_len + payload.len());
    frame.push(1u8); // version
    match addr {
        std::net::SocketAddr::V4(v4) => {
            frame.push(4u8);
            frame.extend_from_slice(&v4.ip().octets());
            frame.extend_from_slice(&v4.port().to_le_bytes());
        }
        std::net::SocketAddr::V6(v6) => {
            frame.push(6u8);
            frame.extend_from_slice(&v6.ip().octets());
            frame.extend_from_slice(&v6.port().to_le_bytes());
        }
    }
    frame.extend_from_slice(payload);
    frame
}

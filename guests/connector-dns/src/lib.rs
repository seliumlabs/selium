//! DNS egress connector system guest.
//!
//! Performs real DNS over UDP/53 on behalf of other guests, exposing name
//! resolution as a typed, capability-gated RPC on a route it registers
//! itself. The connector holds the network authority (`Network + UdpSocket`);
//! resolving guests hold only a channel grant, resolving `dns/resolve`
//! through discovery rather than attaching a runtime-provisioned channel.
//!
//! # Architecture
//!
//! - A raw [`UdpSocket`] talks to the configured upstream resolver.
//! - A [`ResourceListener`] accepts typed [`DnsQuery`] → [`DnsResponse`] RPC
//!   connections (one handler task per connection).
//! - Each query is assigned a transaction id and registered in a shared
//!   [`InFlight`] map; the socket receive loop demuxes upstream replies by
//!   transaction id and drops unknown ones (no cross-talk).
//! - Timeout, NXDOMAIN, truncation, SERVFAIL, REFUSED, and forwarding
//!   failures each surface as a distinct typed [`DnsOutcome`].

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context as _, bail};
use correlate::{InFlight, response_from_parsed};
use parking_lot::Mutex;
use selium_guest::{
    Context, Datagram, Instant, ResourceClass, ResourceListener, ResourceTarget, Serve, Timer,
    UdpSocket, debug, entrypoint, info, mark_ready, spawn, warn,
};
use selium_proto_dns::{DnsOutcome, DnsQuery, DnsResponse, wire};
use selium_shm::rpc::{self, RpcConnection, RpcError};
use tokio::sync::mpsc;

pub mod correlate;

/// How long the connector waits for an upstream reply before surfacing a
/// typed [`DnsOutcome::Timeout`].
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Accepts typed RPC connections and serves one handler per connection.
async fn accept_loop(
    listener: ResourceListener,
    inflight: Arc<InFlight>,
    socket: Arc<Mutex<UdpSocket>>,
    resolver: SocketAddr,
) {
    loop {
        let incoming = match listener.recv().await {
            Ok(incoming) => incoming,
            Err(e) => {
                warn!("dns-connector: accept failed: {e}");
                continue;
            }
        };

        let connection = match rpc::accept::<DnsQuery, DnsResponse>(incoming.into()) {
            Ok(connection) => connection,
            Err(e) => {
                warn!("dns-connector: rpc accept failed: {e}");
                continue;
            }
        };

        spawn(handler(
            connection,
            inflight.clone(),
            socket.clone(),
            resolver,
        ));
    }
}

/// Entrypoint for the DNS connector system guest.
///
/// Receives a discovery `Context` and the upstream resolver address as a
/// pointer argument (`(address, length)` over `udp://<resolver>:53` bytes),
/// binds a raw UDP socket, creates its own listener queue, and self-registers
/// the `dns/resolve` route in the root namespace — permitted only because the
/// connector holds the system-registration capability.
#[entrypoint]
async fn dns_connector(mut ctx: Context, resolver: (u64, u64)) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("dns-connector: starting");

    let Some(resolver_addr) = read_resolver(resolver) else {
        bail!("dns-connector: invalid resolver address argument");
    };

    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .with_context(|| "dns-connector: udp bind failed")?;

    let listener =
        ResourceListener::create().with_context(|| "dns-connector: create listener failed")?;

    // Self-register the `dns/resolve` route under the root namespace. The
    // connector runs as the root/platform tenant, so this requires the
    // system-registration capability (enforced by discovery).
    let target = ResourceTarget {
        uri: String::new(), // pinned by `serve` to the derived internal path
        host_id: String::new(),
        resource_id: listener.descriptor().shared_id,
        interface: None,
        tenant: None,
        class: ResourceClass::HostQueue,
        labels: Vec::new(),
    };
    ctx.serve(Serve {
        path: vec!["dns".to_string(), "resolve".to_string()],
        target,
        default: false,
    })
    .await
    .with_context(|| "dns-connector: serve failed")?;

    mark_ready();

    let socket = Arc::new(Mutex::new(socket));
    let inflight = InFlight::new();

    spawn(recv_loop(
        socket.clone(),
        Arc::new(inflight.clone()),
        resolver_addr,
    ));

    // Keep the poll-owner entrypoint future parked on the accept loop. Under
    // the runtime's process-lifecycle semantics an entrypoint that returns
    // marks its process for teardown, so the connector accepts inline rather
    // than returning after spawning the loops.
    accept_loop(listener, Arc::new(inflight), socket, resolver_addr).await;

    Ok(())
}

/// Serves queries on one connection: forwards to the upstream resolver and
/// replies with the typed outcome.
async fn handler(
    mut connection: RpcConnection<DnsQuery, DnsResponse>,
    inflight: Arc<InFlight>,
    socket: Arc<Mutex<UdpSocket>>,
    resolver: SocketAddr,
) {
    loop {
        let request = match connection.recv().await {
            Ok(request) => request,
            Err(RpcError::ConnectionClosed) => break,
            Err(e) => {
                warn!("dns-connector: recv failed: {e}");
                break;
            }
        };

        let query = match request.payload() {
            Ok(query) => query,
            Err(e) => {
                warn!("dns-connector: query decode failed: {e}");
                continue;
            }
        };

        // Allocate a transaction id and register the reply channel before the
        // wire query goes out, so a fast reply can never be missed.
        let (reply_tx, mut reply_rx) = mpsc::channel(1);
        let txid = inflight.register(reply_tx);

        let payload = match wire::encode_query(&query, txid) {
            Ok(payload) => payload,
            Err(e) => {
                debug!("dns-connector: encode query failed: {e}");
                drop(inflight.take(txid));
                drop(
                    request
                        .reply(DnsResponse::failure(DnsOutcome::Upstream))
                        .await,
                );
                continue;
            }
        };

        if let Err(e) = udp_send(socket.clone(), resolver, payload).await {
            warn!("dns-connector: udp send failed: {e}");
            drop(inflight.take(txid));
            drop(
                request
                    .reply(DnsResponse::failure(DnsOutcome::Upstream))
                    .await,
            );
            continue;
        }

        let deadline = Instant::now()
            .ok()
            .and_then(|now| now.checked_add(RESOLVE_TIMEOUT))
            .unwrap_or(Instant::MAX);

        let response = tokio::select! {
            maybe = reply_rx.recv() => {
                maybe.unwrap_or_else(|| DnsResponse::failure(DnsOutcome::Timeout))
            }
            _ = Timer::new(deadline) => {
                drop(inflight.take(txid));
                DnsResponse::failure(DnsOutcome::Timeout)
            }
        };

        drop(request.reply(response).await);
    }
}

/// Reads and parses the resolver argument into a socket address.
fn read_resolver(resolver: (u64, u64)) -> Option<SocketAddr> {
    // SAFETY: the `(address, length)` pair was written into this guest's
    // linear memory by the runtime for this entrypoint invocation.
    let text = unsafe { selium_guest::args::str(resolver.0, resolver.1) }?;
    let text = text.trim().strip_prefix("udp://").unwrap_or(text);
    text.parse().ok()
}

/// Receives upstream datagrams and demuxes them by transaction id.
async fn recv_loop(socket: Arc<Mutex<UdpSocket>>, inflight: Arc<InFlight>, resolver: SocketAddr) {
    loop {
        let datagram = match udp_recv(socket.clone()).await {
            Ok(datagram) => datagram,
            Err(e) => {
                warn!("dns-connector: udp recv failed: {e}");
                continue;
            }
        };

        // Only replies from the configured resolver are ever demuxed; a
        // datagram from any other source address is spoofing and dropped.
        if datagram.addr != resolver {
            debug!("dns-connector: dropping datagram from unexpected source");
            continue;
        }

        let parsed = match wire::parse_response(&datagram.payload) {
            Ok(parsed) => parsed,
            Err(e) => {
                debug!("dns-connector: malformed reply dropped: {e}");
                continue;
            }
        };

        // Unknown transaction id → nobody asked for this reply → drop it.
        let Some(reply) = inflight.take(parsed.txid) else {
            debug!(
                "dns-connector: dropping reply with unknown txid {}",
                parsed.txid
            );
            continue;
        };

        drop(reply.send(response_from_parsed(&parsed)).await);
    }
}

/// Receives one datagram through the shared socket.
async fn udp_recv(socket: Arc<Mutex<UdpSocket>>) -> selium_guest::Result<Datagram> {
    std::future::poll_fn(move |cx| {
        let mut guard = socket.lock();
        let socket = std::pin::Pin::new(&mut *guard);
        socket.poll_recv(cx)
    })
    .await
}

/// Sends one datagram through the shared socket.
async fn udp_send(
    socket: Arc<Mutex<UdpSocket>>,
    addr: SocketAddr,
    payload: Vec<u8>,
) -> selium_guest::Result<usize> {
    std::future::poll_fn(move |cx| {
        let mut guard = socket.lock();
        let socket = std::pin::Pin::new(&mut *guard);
        socket.poll_send(
            cx,
            &Datagram {
                addr,
                payload: payload.clone(),
            },
        )
    })
    .await
}

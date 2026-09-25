//! QUIC echo demo guest.
//!
//! The app-guest side of the QUIC connector: serves a named route with
//! discovery and echoes every accepted stream's bytes back to the client.
//! The `selium-runtime` `quic_spine` integration test deploys this guest
//! alongside the real QUIC connector guest and an external native quinn
//! client, exercising the full relay path — SNI routing, per-stream byte
//! channels, backpressure, and FIN/EOF lifecycle — inside real WASM guests.
//!
//! The guest holds **no `Network` grants**: QUIC is terminated at the edge by
//! the connector, and only capability-gated shared-memory byte channels reach
//! this guest (see `selium-guest::net::quic`).

use std::time::Duration;

use anyhow::Context as _;
use selium_guest::{
    Context, GuestError, entrypoint, error, info, mark_ready,
    net::quic::{QuicServe, QuicStream},
    spawn,
    time::{Instant, Timer},
    warn,
};

/// Bind attempts before giving up. The connector's Tier-1 `sel-quic` handler
/// registration is published to the discovery feed before this guest boots,
/// but the discovery guest consumes feed events on its own reactor turns, so
/// a registration racing ahead of that consumption is answered `NoHandler` —
/// retry briefly instead of failing the deployment.
const BIND_ATTEMPTS: u8 = 10;
/// The service path this demo serves. The demo runs in the root/system
/// tenant (holding the system-registration capability), so the route is
/// `sel:///localhost` and the projected wire name is the bare label
/// `localhost` — which must match the test client's SNI and the test
/// certificate's SAN.
const SERVE_NAME: &str = "localhost";

/// Attempts to serve the route, retrying briefly on failure.
async fn bind_with_retry(ctx: &mut Context, uri: &str) -> Result<QuicServe, GuestError> {
    for attempt in 1..BIND_ATTEMPTS {
        match QuicServe::bind(ctx, uri).await {
            Ok(serve) => return Ok(serve),
            Err(e) => {
                warn!("quic-demo: bind attempt {attempt}/{BIND_ATTEMPTS} failed: {e}; retrying");
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
    QuicServe::bind(ctx, uri).await
}

/// Echoes one relayed stream.
///
/// Reads until the client's FIN surfaces as EOF on the stream's channel,
/// writes the payload back, then drops the stream: the blocking writer's drop
/// decrements the ring's writer count, so the connector observes ring EOF and
/// finishes the QUIC stream on the wire (the client sees its FIN).
async fn echo_stream(stream: QuicStream) {
    let mut stream = stream;
    let mut payload = Vec::new();
    if let Err(e) = tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut payload).await {
        error!("quic-demo: stream read failed: {e}");
        return;
    }
    let len = payload.len();
    info!("quic-demo: eof after {len} bytes, echoing");
    if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut stream, &payload).await {
        error!("quic-demo: stream write failed: {e}");
        return;
    }
    info!("quic-demo: echo written, closing");
    drop(stream);
    info!("quic-demo: echoed {len} bytes");
}

/// Serves QUIC byte streams relayed by the connector: register the route with
/// discovery, then echo each accepted stream.
#[entrypoint]
async fn quic_demo(ctx: Context) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("quic-demo: booting");

    let mut ctx = ctx;
    let mut serve = bind_with_retry(&mut ctx, SERVE_NAME)
        .await
        .with_context(|| "quic-demo: bind failed")?;
    info!("quic-demo: bound {SERVE_NAME}");
    mark_ready();

    loop {
        let stream = serve
            .accept()
            .await
            .with_context(|| "quic-demo: accept failed")?;
        info!("quic-demo: accepted stream");
        spawn(echo_stream(stream));
    }
}

/// Sleeps for `duration` via the guest `Sleep` hostcall.
async fn sleep(duration: Duration) {
    let deadline = match Instant::now() {
        Ok(now) => now.checked_add(duration).unwrap_or(Instant::MAX),
        Err(_) => Instant::MAX,
    };
    Timer::new(deadline).await;
}

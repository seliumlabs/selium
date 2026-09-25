//! DNS resolution demo guest.
//!
//! Resolves a name through the DNS connector route (`dns/resolve`, registered
//! by the connector itself), and then connects to the resolved literal —
//! exercising the resolution data path end-to-end inside a real WASM guest.
//! The `selium-runtime` `dns_spine` integration test deploys this guest
//! against a loopback fake resolver and asserts on its log output.

use std::net::SocketAddr;

use anyhow::{Context as _, bail};
use selium_guest::{Context, TcpStream, entrypoint, error, info, mark_ready};

/// The name the demo resolves.
const DEMO_NAME: &str = "example.test";

/// Reads the target `ip:port` from a pointer argument.
fn read_connect_addr(connect: (u64, u64)) -> Option<SocketAddr> {
    // SAFETY: the `(address, length)` pair was written into this guest's
    // linear memory by the runtime for this entrypoint invocation.
    let text = unsafe { selium_guest::args::str(connect.0, connect.1) }?;
    text.trim().parse().ok()
}

#[entrypoint]
async fn resolve_demo(mut ctx: Context, connect: (u64, u64)) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("dns-demo: booting");

    // Resolve the name through the DNS connector route in discovery.
    let addresses = selium_guest::net::resolve(&mut ctx, DEMO_NAME)
        .await
        .with_context(|| "dns-demo: resolve failed")?;

    for address in &addresses {
        info!("resolved {} -> {}", DEMO_NAME, address);
    }

    // Then connect to the resolved literal (the name's A record points at
    // loopback; the TCP test server listens on that address).
    let Some(connect_addr) = read_connect_addr(connect) else {
        bail!("dns-demo: invalid connect address argument");
    };

    match TcpStream::connect(&connect_addr.to_string()).await {
        Ok(_stream) => info!("connected to {}", connect_addr),
        Err(e) => error!("dns-demo: connect failed: {e}"),
    }

    mark_ready();
    Ok(())
}

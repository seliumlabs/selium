//! Network demo guest — event-driven proxy integration fixture.
//!
//! Used by the `selium-runtime` `net_wake` integration test to verify the
//! WaitRegister wake bridge and the stall kick end-to-end through a real
//! WASM guest:
//!
//! 1. Binds a listener (the test discovers the bound port via the runtime).
//! 2. Accepts one connection and parks a read on its inbound ring — this
//!    issues a `WaitRegister` hostcall for the parked task.
//! 3. When data arrives, logs `read done`, echoes the bytes back on the
//!    outbound ring, then parks on a second read so the reactor stalls
//!    right after having written outbound frames.

use anyhow::{Context as _, bail};
use selium_guest::{
    entrypoint, error, info,
    net::tcp::{TcpListener, TcpStream},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[entrypoint]
async fn net_demo() -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("net-demo started");

    let listener = TcpListener::bind("127.0.0.1:0").with_context(|| "net-demo: bind failed")?;
    // Readiness anchor for the integration test.
    info!("net-demo: bound");

    let mut stream: TcpStream = listener
        .accept()
        .await
        .with_context(|| "net-demo: accept failed")?;
    info!("net-demo: accepted");

    // Park on the inbound ring until the test writes request bytes. The
    // wake must come from the host's WaitRegister/mailbox bridge, not from
    // any guest-side polling.
    let mut buf = [0_u8; 64];
    match stream.read(&mut buf).await {
        Ok(0) => {
            bail!("net-demo: unexpected EOF before request");
        }
        Ok(n) => {
            info!("net-demo: read done ({n} bytes)");
            let Some(chunk) = buf.get(..n) else {
                bail!("net-demo: read returned out-of-bounds length {n}");
            };
            stream
                .write_all(chunk)
                .await
                .with_context(|| "net-demo: echo write failed")?;
            drop(stream.flush().await);
        }
        Err(e) => {
            return Err(anyhow::anyhow!("net-demo: read failed: {e}"));
        }
    }

    info!("net-demo: echoed");

    // Stall the reactor immediately after writing outbound frames: the
    // runtime's stall kick (not the bounded backstop) must drain them.
    let mut buf2 = [0_u8; 64];
    match stream.read(&mut buf2).await {
        Ok(n) => info!("net-demo: second read done ({n} bytes)"),
        Err(e) => error!("net-demo: second read failed: {e}"),
    }

    Ok(())
}

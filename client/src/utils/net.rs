use anyhow::{Context, Result};
use std::net::{SocketAddr, ToSocketAddrs};

pub(crate) fn get_socket_addrs(host: &str) -> Result<SocketAddr> {
    let addr = host
        .to_socket_addrs()?
        .next()
        .context("Address not available")?;

    Ok(addr)
}

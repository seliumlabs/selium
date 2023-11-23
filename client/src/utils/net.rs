use selium_std::errors::{ParseRemoteAddressError, Result};
use std::net::{SocketAddr, ToSocketAddrs};

pub(crate) fn get_socket_addrs(addr: &str) -> Result<SocketAddr> {
    let addr = addr
        .to_socket_addrs()
        .map_err(ParseRemoteAddressError::InvalidAddress)?
        .next()
        .ok_or(ParseRemoteAddressError::NoAddressResolved)?;

    Ok(addr)
}

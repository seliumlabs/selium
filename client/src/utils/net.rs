use selium_std::errors::{Result, SeliumError};
use std::net::{SocketAddr, ToSocketAddrs};

pub(crate) fn get_socket_addrs(host: &str) -> Result<SocketAddr> {
    let addr = host
        .to_socket_addrs()
        .map_err(|_| SeliumError::ParseSocketAddressError)?
        .next()
        .ok_or(SeliumError::ParseSocketAddressError)?;

    Ok(addr)
}

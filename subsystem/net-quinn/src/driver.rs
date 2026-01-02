use std::sync::Arc;

use futures_util::future::BoxFuture;
use quinn::rustls::sign;
use quinn::{RecvStream, SendStream};
use selium_abi::IoFrame;
use selium_kernel::{
    drivers::{io::IoCapability, net::NetCapability},
    guest_data::GuestError,
};
use tracing::debug;

use crate::{Listener, ListenerRegistry, QuinnError, connect_remote};

/// Handle for a bound QUIC listener.
#[derive(Clone)]
pub struct ListenerHandle {
    listener: Arc<Listener>,
    domain: String,
}

impl ListenerHandle {
    pub(crate) fn new(listener: Arc<Listener>, domain: String) -> Self {
        Self { listener, domain }
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }
}

/// Quinn-backed network driver.
pub struct QuinnDriver {
    registry: Arc<ListenerRegistry>,
}

impl QuinnDriver {
    /// Create a new driver instance with an already validated certificate and private key.
    pub fn new(certified_key: Arc<sign::CertifiedKey>) -> Arc<Self> {
        Arc::new(Self {
            registry: Arc::new(ListenerRegistry::new(certified_key)),
        })
    }
}

impl NetCapability for QuinnDriver {
    type Handle = ListenerHandle;
    type Reader = RecvStream;
    type Writer = SendStream;
    type Error = QuinnError;

    fn create(&self, domain: &str, port: u16) -> BoxFuture<'_, Result<Self::Handle, Self::Error>> {
        let domain = domain.to_owned();
        let registry = Arc::clone(&self.registry);

        Box::pin(async move {
            let listener = registry.get_or_try_init(port)?;
            listener.ensure_domain(&domain)?;
            Ok(ListenerHandle::new(listener, domain))
        })
    }

    fn connect(
        &self,
        domain: &str,
        port: u16,
    ) -> BoxFuture<'_, Result<(Self::Reader, Self::Writer, String), Self::Error>> {
        let domain = domain.to_owned();

        Box::pin(async move {
            let (reader, writer, remote_addr) = connect_remote(&domain, port).await?;
            Ok((reader, writer, format!("{remote_addr}")))
        })
    }

    fn accept(
        &self,
        handle: &Self::Handle,
    ) -> BoxFuture<'_, Result<(Self::Reader, Self::Writer, String), Self::Error>> {
        let listener = Arc::clone(&handle.listener);
        let domain = handle.domain.clone();

        Box::pin(async move {
            let (reader, writer, remote_addr) = listener.accept_for_domain(&domain).await?;
            Ok((reader, writer, format!("{remote_addr}")))
        })
    }
}

impl IoCapability for QuinnDriver {
    type Handle = ();
    type Reader = RecvStream;
    type Writer = SendStream;
    type Error = QuinnError;

    fn new_writer(&self, _handle: &Self::Handle) -> Result<Self::Writer, Self::Error> {
        Err(QuinnError::Unsupported)
    }

    fn new_reader(&self, _handle: &Self::Handle) -> Result<Self::Reader, Self::Error> {
        Err(QuinnError::Unsupported)
    }

    async fn read(&self, reader: &mut Self::Reader, len: usize) -> Result<IoFrame, Self::Error> {
        let mut buf = vec![0u8; len];
        let read = reader.read(&mut buf).await.map_err(QuinnError::Read)?;
        let bytes_read = read.unwrap_or(0);
        buf.truncate(bytes_read);
        debug!(bytes_read, requested = len, "quinn read");
        Ok(IoFrame {
            writer_id: 0,
            payload: buf,
        })
    }

    async fn write(&self, writer: &mut Self::Writer, bytes: &[u8]) -> Result<(), Self::Error> {
        debug!(bytes = bytes.len(), "quinn write");
        writer.write_all(bytes).await.map_err(QuinnError::Write)?;
        Ok(())
    }
}

impl From<QuinnError> for GuestError {
    fn from(value: QuinnError) -> Self {
        match value {
            QuinnError::PortRange => GuestError::InvalidArgument,
            QuinnError::Resolve { .. } => GuestError::InvalidArgument,
            _ => GuestError::Subsystem(value.to_string()),
        }
    }
}

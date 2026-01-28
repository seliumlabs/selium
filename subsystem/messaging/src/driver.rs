use std::sync::Arc;

use selium_abi::{ChannelBackpressure, IoFrame};
use selium_kernel::{
    drivers::{channel::ChannelCapability, io::IoCapability},
    guest_data::{GuestError, GuestUint},
};
use tokio::io::AsyncWriteExt;

use crate::{Channel, ChannelError, StrongReader, StrongWriter, WeakReader, WeakWriter};

/// Runtime driver for channel hostcalls
#[derive(Clone)]
pub struct ChannelDriver;

/// Runtime driver for strong read/write hostcalls
pub struct ChannelStrongIoDriver;

/// Runtime driver for weak read/write hostcalls
pub struct ChannelWeakIoDriver;

impl ChannelDriver {
    /// Create a new channel driver instance
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl ChannelCapability for ChannelDriver {
    type Channel = Arc<Channel>;
    type StrongReader = StrongReader;
    type WeakReader = WeakReader;
    type StrongWriter = StrongWriter;
    type WeakWriter = WeakWriter;
    type Error = ChannelError;

    fn create(
        &self,
        size: GuestUint,
        backpressure: ChannelBackpressure,
    ) -> Result<Self::Channel, Self::Error> {
        let backpressure = match backpressure {
            ChannelBackpressure::Park => crate::Backpressure::Park,
            ChannelBackpressure::Drop => crate::Backpressure::Drop,
        };
        Ok(Channel::with_parameters(size as usize, backpressure))
    }

    fn delete(&self, channel: Self::Channel) -> Result<(), Self::Error> {
        channel.terminate()
    }

    fn drain(&self, channel: &Self::Channel) -> Result<(), Self::Error> {
        channel.drain()
    }

    fn downgrade_writer(
        &self,
        writer: Self::StrongWriter,
    ) -> Result<Self::WeakWriter, Self::Error> {
        Ok(writer.downgrade())
    }

    fn downgrade_reader(
        &self,
        reader: Self::StrongReader,
    ) -> Result<Self::WeakReader, Self::Error> {
        Ok(reader.downgrade())
    }

    fn ptr(&self, channel: &Self::Channel) -> String {
        format!("{:p}", Arc::as_ptr(channel))
    }
}

impl ChannelStrongIoDriver {
    /// Create a new channel strong I/O driver instance
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl IoCapability for ChannelStrongIoDriver {
    type Handle = Arc<Channel>;
    type Reader = StrongReader;
    type Writer = StrongWriter;
    type Error = ChannelError;

    fn new_reader(&self, handle: &Self::Handle) -> Result<Self::Reader, Self::Error> {
        Ok(handle.new_strong_reader())
    }

    fn new_writer(&self, handle: &Self::Handle) -> Result<Self::Writer, Self::Error> {
        Ok(handle.new_strong_writer())
    }

    async fn read(&self, reader: &mut Self::Reader, len: usize) -> Result<IoFrame, Self::Error> {
        let (id, buf) = reader.read_frame(len).await?;
        Ok(IoFrame {
            writer_id: id,
            payload: buf,
        })
    }

    async fn write(&self, writer: &mut Self::Writer, bytes: &[u8]) -> Result<(), Self::Error> {
        let mut offset = 0;
        while offset < bytes.len() {
            let written = writer.write(&bytes[offset..]).await?;
            if written == 0 {
                if offset == 0 {
                    return Ok(());
                }
                return Err(ChannelError::Io("write stalled mid-frame".to_string()));
            }
            offset += written;
        }
        Ok(())
    }
}

impl ChannelWeakIoDriver {
    /// Create a new channel weak I/O driver instance
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl IoCapability for ChannelWeakIoDriver {
    type Handle = Arc<Channel>;
    type Reader = WeakReader;
    type Writer = WeakWriter;
    type Error = ChannelError;

    fn new_reader(&self, handle: &Self::Handle) -> Result<Self::Reader, Self::Error> {
        Ok(handle.new_weak_reader())
    }

    fn new_writer(&self, handle: &Self::Handle) -> Result<Self::Writer, Self::Error> {
        Ok(handle.new_weak_writer())
    }

    async fn read(&self, reader: &mut Self::Reader, len: usize) -> Result<IoFrame, Self::Error> {
        let (id, buf) = reader.read_frame(len).await?;
        Ok(IoFrame {
            writer_id: id,
            payload: buf,
        })
    }

    async fn write(&self, writer: &mut Self::Writer, bytes: &[u8]) -> Result<(), Self::Error> {
        let mut offset = 0;
        while offset < bytes.len() {
            let written = writer.write(&bytes[offset..]).await?;
            if written == 0 {
                if offset == 0 {
                    return Ok(());
                }
                return Err(ChannelError::Io("write stalled mid-frame".to_string()));
            }
            offset += written;
        }
        Ok(())
    }
}

impl From<ChannelError> for GuestError {
    fn from(value: ChannelError) -> Self {
        GuestError::Subsystem(value.to_string())
    }
}

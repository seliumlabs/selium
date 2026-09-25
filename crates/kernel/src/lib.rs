//! Selium kernel primitives.

pub use backend::KernelBackend;
pub use error::{Error, Result};
pub use host_queue::HostQueueRegistry;
pub use kernel::Kernel;
pub use memory::MemoryRegistry;
pub use network::{
    NetworkState, TcpListenerState, TcpStreamState, UdpSocketState, decode_udp_frame,
    encode_udp_frame,
};
pub use poller::{BandwidthFn, Poller};
pub use process::ProcessTable;
pub use quota::QuotaTable;
pub use storage::StorageRegistry;

mod backend;
mod error;
mod host_queue;
mod kernel;
mod memory;
mod network;
mod network_runtime;
mod os_wait_word;
mod poller;
mod process;
mod quota;
mod storage;

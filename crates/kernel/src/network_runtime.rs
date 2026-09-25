//! Kernel network resource lifecycle (close methods).
//!
//! Bind, connect, and proxy functions live in the async runtime
//! (`selium-runtime/src/network.rs`). The kernel retains only the
//! resource-cleanup entry points, which are called from the runtime's
//! process teardown path.

use std::sync::atomic::Ordering;

use crate::{Error, Result, kernel::Kernel};

impl Kernel {
    pub fn close_tcp_listener(&self, local_id: u64) -> Result<()> {
        let mut listeners = self.inner.network.inner.tcp_listeners.lock();
        let state = listeners
            .remove(&local_id)
            .ok_or_else(|| Error::NotFound(format!("tcp listener {local_id}")))?;
        state.running.store(false, Ordering::Relaxed);
        let shared_id = state.shared_id;
        drop(listeners);

        if let Some(queue) = self
            .inner
            .queues
            .inner
            .queues_by_shared
            .lock()
            .get(&shared_id)
            .cloned()
        {
            queue.notify.notify_all();
        }

        self.inner
            .queues
            .inner
            .queues_by_shared
            .lock()
            .remove(&shared_id);
        self.inner
            .queues
            .inner
            .local_queues
            .lock()
            .remove(&local_id);
        Ok(())
    }

    pub fn close_tcp_stream(&self, shared_id: u64) -> Result<()> {
        let state = self
            .inner
            .network
            .inner
            .tcp_streams
            .lock()
            .remove(&shared_id)
            .ok_or_else(|| Error::NotFound(format!("tcp stream {shared_id}")))?;
        state.running.store(false, Ordering::Release);
        Ok(())
    }

    pub fn close_udp_socket(&self, shared_id: u64) -> Result<()> {
        let state = self
            .inner
            .network
            .inner
            .udp_sockets
            .lock()
            .remove(&shared_id)
            .ok_or_else(|| Error::NotFound(format!("udp socket {shared_id}")))?;
        state.running.store(false, Ordering::Release);
        Ok(())
    }
}

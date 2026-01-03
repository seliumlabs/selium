use std::sync::Arc;

use tokio::{select, sync::Notify};
use wasmtime::{Caller, Linker};

use crate::{KernelError, mailbox::GuestMailbox, registry::InstanceRegistry};

/// Host-side support for guest async helpers.
pub struct GuestAsync {
    shutdown: Arc<Notify>,
}

impl GuestAsync {
    /// Create a new guest async capability.
    pub fn new(notify: Arc<Notify>) -> Self {
        Self { shutdown: notify }
    }

    /// Link the `selium::async` host functions into the Wasmtime linker.
    pub fn link(&self, linker: &mut Linker<InstanceRegistry>) -> Result<(), KernelError> {
        let shutdown = Arc::clone(&self.shutdown);
        linker.func_wrap_async(
            "selium::async",
            "yield_now",
            move |caller: Caller<'_, InstanceRegistry>, ()| {
                let mailbox_ref: &'static GuestMailbox =
                    caller.data().mailbox().expect("guest mailbox missing");
                let shutdown = Arc::clone(&shutdown);
                Box::new(async move {
                    loop {
                        if mailbox_ref.is_closed() || mailbox_ref.is_signalled() {
                            break;
                        }
                        select! {
                            _ = shutdown.notified() => {
                                break;
                            }
                            _ = mailbox_ref.wait_for_signal() => {}
                        }
                    }
                })
            },
        )?;
        Ok(())
    }

    /// Signal all waiting guests that the host is shutting down.
    pub fn shutdown(&self) {
        self.shutdown.notify_waiters();
    }
}

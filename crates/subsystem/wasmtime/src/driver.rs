use std::sync::Arc;

use selium_abi::{AbiValue, EntrypointInvocation};
use selium_kernel::{
    drivers::{
        Capability,
        module_store::ModuleStoreReadCapability,
        process::{ProcessInstance, ProcessLifecycleCapability},
    },
    guest_data::GuestError,
    registry::{Registry, ResourceId},
};
use tokio::task::JoinHandle;
use wasmtime::Module;

use crate::{Error, WasmRuntime};

#[derive(Clone)]
pub struct WasmtimeDriver {
    runtime: Arc<WasmRuntime>,
    store: Arc<dyn ModuleStoreReadCapability + Send + Sync>,
}

impl WasmtimeDriver {
    pub fn new(
        runtime: Arc<WasmRuntime>,
        store: Arc<dyn ModuleStoreReadCapability + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self { runtime, store })
    }
}

impl ProcessLifecycleCapability for WasmtimeDriver {
    type Process = ProcessInstance<JoinHandle<Result<Vec<AbiValue>, wasmtime::Error>>>;
    type Error = Error;

    fn start(
        &self,
        registry: &Arc<Registry>,
        process_id: ResourceId,
        module_id: &str,
        name: &str,
        capabilities: Vec<Capability>,
        entrypoint: EntrypointInvocation,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let inner = self.clone();

        async move {
            let bytes = inner.store.read(module_id)?;
            let module = Module::from_binary(&inner.runtime.engine, &bytes)?;
            inner
                .runtime
                .run(
                    registry,
                    process_id,
                    module,
                    name,
                    &capabilities,
                    entrypoint,
                )
                .await
        }
    }

    async fn stop(&self, instance: &mut Self::Process) -> Result<(), Self::Error> {
        instance.inner_mut().abort();
        Ok(())
    }
}

impl From<Error> for GuestError {
    fn from(value: Error) -> Self {
        Self::Subsystem(value.to_string())
    }
}

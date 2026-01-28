//! Guest-facing helpers for spawning and stopping Selium processes.
//!
//! # Examples
//! ```no_run
//! use selium_userland::{
//!     abi::{AbiParam, AbiScalarType, AbiSignature},
//!     process::{Capability, ProcessBuilder, ProcessError},
//! };
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let rt = tokio::runtime::Builder::new_current_thread().build()?;
//!     rt.block_on(async {
//!         let signature =
//!             AbiSignature::new(vec![AbiParam::Scalar(AbiScalarType::I32)], Vec::new());
//!         let builder = ProcessBuilder::new("selium.examples.echo", "echoer")
//!             .capability(Capability::ChannelReader)
//!             .capability(Capability::ChannelWriter)
//!             .signature(signature)
//!             .arg_resource(7u64);
//!         let builder = builder.log_uri("sel://logs/echoer");
//!         let handle = builder.start().await?;
//!
//!         handle.stop().await?;
//!         Ok::<_, ProcessError>(())
//!     })?;
//!     Ok(())
//! }
//! ```
use selium_abi::AbiParam;
use selium_abi::GuestResourceId;
use selium_abi::{
    AbiScalarValue, AbiSignature, EntrypointArg, EntrypointInvocation, ProcessLogLookup,
    ProcessLogRegistration, ProcessStart, RkyvEncode,
};

use crate::driver::{self, DriverFuture, RkyvDecoder, encode_args};
use crate::io::SharedChannel;

pub use selium_abi::Capability;

/// Error returned by process lifecycle helpers.
pub type ProcessError = driver::DriverError;

/// Builder for configuring and launching a Selium process.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessBuilder {
    module_id: String,
    entrypoint: String,
    capabilities: Vec<Capability>,
    signature: AbiSignature,
    args: Vec<EntrypointArg>,
    log_uri: Option<String>,
}

impl ProcessBuilder {
    /// Create a new builder using the module identifier and friendly process name.
    pub fn new(module_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            module_id: module_id.into(),
            entrypoint: name.into(),
            capabilities: vec![Capability::ChannelLifecycle, Capability::ChannelWriter],
            signature: AbiSignature::new(Vec::new(), Vec::new()),
            args: Vec::new(),
            log_uri: None,
        }
    }

    /// Add a capability that the launched process should receive.
    pub fn capability(mut self, capability: Capability) -> Self {
        if !self.capabilities.contains(&capability) {
            self.capabilities.push(capability);
        }
        self
    }

    /// Specify the entrypoint ABI signature.
    ///
    /// The log URI buffer is injected ahead of these params.
    pub fn signature(mut self, signature: AbiSignature) -> Self {
        self.signature = signature;
        self
    }

    /// Set the log URI passed to the entrypoint.
    ///
    /// The value must not be empty.
    pub fn log_uri(mut self, value: impl Into<String>) -> Self {
        self.log_uri = Some(value.into());
        self
    }

    /// Append a scalar argument.
    pub fn arg_scalar(mut self, value: AbiScalarValue) -> Self {
        self.args.push(EntrypointArg::Scalar(value));
        self
    }

    /// Append a 32-bit integer argument.
    pub fn arg_i32(self, value: i32) -> Self {
        self.arg_scalar(AbiScalarValue::I32(value))
    }

    /// Append a 64-bit integer argument.
    pub fn arg_i64(self, value: i64) -> Self {
        self.arg_scalar(AbiScalarValue::I64(value))
    }

    /// Append a 32-bit float argument.
    pub fn arg_f32(self, value: f32) -> Self {
        self.arg_scalar(AbiScalarValue::F32(value))
    }

    /// Append a 64-bit float argument.
    pub fn arg_f64(self, value: f64) -> Self {
        self.arg_scalar(AbiScalarValue::F64(value))
    }

    /// Append a UTF-8 string argument.
    pub fn arg_utf8(self, value: impl Into<String>) -> Self {
        self.arg_buffer(value.into().into_bytes())
    }

    /// Append an rkyv-encoded argument.
    pub fn arg_rkyv<T: RkyvEncode>(mut self, value: &T) -> Result<Self, ProcessError> {
        let bytes = encode_args(value)?;
        self.args.push(EntrypointArg::Buffer(bytes));
        Ok(self)
    }

    /// Append a raw buffer argument.
    pub fn arg_buffer(mut self, value: impl Into<Vec<u8>>) -> Self {
        self.args.push(EntrypointArg::Buffer(value.into()));
        self
    }

    /// Append a resource handle argument.
    pub fn arg_resource(mut self, handle: impl Into<GuestResourceId>) -> Self {
        self.args.push(EntrypointArg::Resource(handle.into()));
        self
    }

    /// Launch the configured process and return its handle.
    pub async fn start(self) -> Result<ProcessHandle, ProcessError> {
        start_process(self).await
    }
}

/// Handle representing a running process in the Selium registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessHandle(GuestResourceId);

impl ProcessHandle {
    /// Access the underlying registry handle.
    pub fn raw(&self) -> GuestResourceId {
        self.0
    }

    /// Construct a handle from a raw registry identifier.
    ///
    /// # Safety
    /// The handle must be a valid process capability minted for the current guest. Forged or stale
    /// handles may be rejected by the host kernel or cause undefined behaviour.
    pub unsafe fn from_raw(handle: GuestResourceId) -> Self {
        Self(handle)
    }

    /// Stop the referenced process.
    pub async fn stop(self) -> Result<(), ProcessError> {
        let args = encode_args(&self.0)?;
        DriverFuture::<process_stop::Module, RkyvDecoder<()>>::new(&args, 0, RkyvDecoder::new())?
            .await
            .map(|_| ())
    }

    /// Fetch the shared logging channel registered by this process.
    pub async fn log_channel(&self) -> Result<SharedChannel, ProcessError> {
        let args = encode_args(&ProcessLogLookup { process_id: self.0 })?;
        let handle =
            DriverFuture::<process_log_channel::Module, RkyvDecoder<GuestResourceId>>::new(
                &args,
                8,
                RkyvDecoder::new(),
            )?
            .await?;

        // Safe because the handle is minted by the host kernel.
        Ok(unsafe { SharedChannel::from_raw(handle) })
    }
}

/// Register the supplied shared channel as the logging stream for the current process.
pub async fn register_log_channel(reference: SharedChannel) -> Result<(), ProcessError> {
    let args = encode_args(&ProcessLogRegistration {
        channel: reference.raw(),
    })?;
    DriverFuture::<process_register_log::Module, RkyvDecoder<()>>::new(
        &args,
        0,
        RkyvDecoder::new(),
    )?
    .await
    .map(|_| ())
}

async fn start_process(builder: ProcessBuilder) -> Result<ProcessHandle, ProcessError> {
    let args = encode_start_args(builder)?;
    let handle = DriverFuture::<process_start::Module, RkyvDecoder<GuestResourceId>>::new(
        &args,
        8,
        RkyvDecoder::new(),
    )?
    .await?;
    Ok(ProcessHandle(handle))
}

fn encode_start_args(builder: ProcessBuilder) -> Result<Vec<u8>, ProcessError> {
    let payload = build_start_payload(builder)?;
    encode_args(&payload)
}

fn build_start_payload(builder: ProcessBuilder) -> Result<ProcessStart, ProcessError> {
    let ProcessBuilder {
        module_id,
        entrypoint: entrypoint_name,
        capabilities,
        signature,
        args,
        log_uri,
    } = builder;

    let (signature, args) = inject_log_uri(signature, args, log_uri)?;

    let entrypoint = EntrypointInvocation::new(signature.clone(), args)
        .map_err(|_| ProcessError::InvalidArgument)?;

    Ok(ProcessStart {
        module_id,
        name: entrypoint_name,
        capabilities,
        entrypoint,
    })
}

fn inject_log_uri(
    signature: AbiSignature,
    args: Vec<EntrypointArg>,
    log_uri: Option<String>,
) -> Result<(AbiSignature, Vec<EntrypointArg>), ProcessError> {
    let log_uri = match log_uri {
        Some(value) if value.is_empty() => return Err(ProcessError::InvalidArgument),
        Some(value) => value,
        None => String::new(),
    };
    let mut params = Vec::with_capacity(signature.params().len() + 1);
    params.push(AbiParam::Buffer);
    params.extend_from_slice(signature.params());
    let signature = AbiSignature::new(params, signature.results().to_vec());

    let mut args_with_uri = Vec::with_capacity(args.len() + 1);
    args_with_uri.push(EntrypointArg::Buffer(log_uri.into_bytes()));
    args_with_uri.extend(args);

    Ok((signature, args_with_uri))
}

driver_module!(process_start, PROCESS_START, "selium::process::start");
driver_module!(process_stop, PROCESS_STOP, "selium::process::stop");
driver_module!(
    process_register_log,
    PROCESS_REGISTER_LOG,
    "selium::process::register_log_channel"
);
driver_module!(
    process_log_channel,
    PROCESS_LOG_CHANNEL,
    "selium::process::log_channel"
);

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::decode_rkyv;
    use selium_abi::{AbiParam, AbiScalarType};

    #[test]
    fn encode_start_args_serialises_signature_and_arguments() {
        let signature = AbiSignature::new(
            vec![AbiParam::Scalar(AbiScalarType::I32), AbiParam::Buffer],
            vec![AbiParam::Scalar(AbiScalarType::F64)],
        );

        let builder = ProcessBuilder::new("module", "proc")
            .capability(Capability::ChannelLifecycle)
            .capability(Capability::ChannelReader)
            .signature(signature.clone())
            .arg_i32(42)
            .arg_buffer([1, 2, 3]);
        let bytes = encode_start_args(builder).expect("encode");
        let start = decode_rkyv::<ProcessStart>(&bytes).expect("decode");
        assert_eq!(start.module_id, "module");
        assert_eq!(start.name, "proc");
        assert_eq!(
            start.capabilities,
            vec![
                Capability::ChannelLifecycle,
                Capability::ChannelWriter,
                Capability::ChannelReader
            ]
        );
        assert_eq!(start.entrypoint.signature.params()[0], AbiParam::Buffer);
        assert_eq!(
            start.entrypoint.signature.params()[1..],
            *signature.params()
        );
        assert_eq!(start.entrypoint.signature.results(), signature.results());
        assert_eq!(start.entrypoint.args[0], EntrypointArg::Buffer(Vec::new()));
        assert_eq!(
            start.entrypoint.args[1..],
            [
                EntrypointArg::Scalar(AbiScalarValue::I32(42)),
                EntrypointArg::Buffer(vec![1, 2, 3])
            ]
        );
    }

    #[test]
    fn encode_start_args_supports_resources() {
        let signature = AbiSignature::new(vec![AbiParam::Scalar(AbiScalarType::I32)], Vec::new());
        let builder = ProcessBuilder::new("module", "proc")
            .signature(signature)
            .arg_resource(7u64);
        let bytes = encode_start_args(builder).expect("encode");
        let start = decode_rkyv::<ProcessStart>(&bytes).expect("decode");
        assert_eq!(start.entrypoint.args[0], EntrypointArg::Buffer(Vec::new()));
        assert_eq!(start.entrypoint.args[1..], [EntrypointArg::Resource(7)]);
    }

    #[test]
    fn encode_start_args_supports_shared_resources() {
        let signature = AbiSignature::new(vec![AbiParam::Scalar(AbiScalarType::U64)], Vec::new());
        let builder = ProcessBuilder::new("module", "proc")
            .signature(signature.clone())
            .arg_resource(7u64);
        let bytes = encode_start_args(builder).expect("encode");
        let start = decode_rkyv::<ProcessStart>(&bytes).expect("decode");
        assert_eq!(start.entrypoint.signature.params()[0], AbiParam::Buffer);
        assert_eq!(
            start.entrypoint.signature.params()[1..],
            *signature.params()
        );
        assert_eq!(start.entrypoint.signature.results(), signature.results());
        assert_eq!(start.entrypoint.args[0], EntrypointArg::Buffer(Vec::new()));
        assert_eq!(start.entrypoint.args[1..], [EntrypointArg::Resource(7)]);
    }

    #[test]
    fn encode_start_args_allows_missing_log_uri() {
        let signature = AbiSignature::new(Vec::new(), Vec::new());
        let builder = ProcessBuilder::new("module", "proc").signature(signature);
        let bytes = encode_start_args(builder).expect("encode");
        let start = decode_rkyv::<ProcessStart>(&bytes).expect("decode");
        assert_eq!(start.entrypoint.signature.params()[0], AbiParam::Buffer);
        assert_eq!(start.entrypoint.args[0], EntrypointArg::Buffer(Vec::new()));
    }
}

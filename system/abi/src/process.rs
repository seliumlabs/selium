use rkyv::{Archive, Deserialize, Serialize};

use crate::{
    AbiParam, AbiScalarType, AbiScalarValue, AbiSignature, CallPlanError, GuestResourceId,
};

/// Argument supplied to a process entrypoint.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum EntrypointArg {
    /// Immediate scalar value.
    Scalar(AbiScalarValue),
    /// Raw buffer.
    Buffer(Vec<u8>),
    /// Handle referring to a Selium resource.
    Resource(GuestResourceId),
}

/// Invocation of a process entrypoint.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct EntrypointInvocation {
    /// ABI signature describing the entrypoint.
    pub signature: AbiSignature,
    /// Concrete arguments supplied by the caller.
    pub args: Vec<EntrypointArg>,
}

impl EntrypointInvocation {
    /// Construct an invocation, validating that arguments satisfy the signature.
    pub fn new(signature: AbiSignature, args: Vec<EntrypointArg>) -> Result<Self, CallPlanError> {
        let invocation = Self { signature, args };
        invocation.validate()?;
        Ok(invocation)
    }

    /// Validate that arguments align with the ABI signature.
    pub fn validate(&self) -> Result<(), CallPlanError> {
        if self.signature.params().len() != self.args.len() {
            return Err(CallPlanError::ParameterCount {
                expected: self.signature.params().len(),
                actual: self.args.len(),
            });
        }

        for (index, (param, arg)) in self
            .signature
            .params()
            .iter()
            .zip(self.args.iter())
            .enumerate()
        {
            match (param, arg) {
                (AbiParam::Scalar(expected), EntrypointArg::Scalar(actual)) => {
                    if actual.kind() != *expected {
                        return Err(CallPlanError::ValueMismatch {
                            index,
                            reason: "scalar type mismatch",
                        });
                    }
                }
                (AbiParam::Scalar(AbiScalarType::I32), EntrypointArg::Resource(_))
                | (AbiParam::Scalar(AbiScalarType::U64), EntrypointArg::Resource(_)) => {}
                (AbiParam::Buffer, EntrypointArg::Buffer(_)) => {}
                _ => {
                    return Err(CallPlanError::ValueMismatch {
                        index,
                        reason: "argument incompatible with signature",
                    });
                }
            }
        }

        Ok(())
    }

    /// Access the invocation signature.
    pub fn signature(&self) -> &AbiSignature {
        &self.signature
    }
}

/// Register a process's logging channel with the host.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct ProcessLogRegistration {
    /// Shared channel handle exported by the guest.
    pub channel: GuestResourceId,
}

/// Request the logging channel for a running process.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct ProcessLogLookup {
    /// Handle referencing the process to inspect.
    pub process_id: GuestResourceId,
}

/// Request to start a new process instance.
#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct ProcessStart {
    /// Module identifier that should be activated.
    pub module_id: String,
    /// Friendly process name.
    pub name: String,
    /// Capabilities granted to the process.
    pub capabilities: Vec<crate::Capability>,
    /// Entrypoint invocation details.
    pub entrypoint: EntrypointInvocation,
}

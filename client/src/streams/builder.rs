use crate::traits::TryIntoU64;
use anyhow::Result;
use quinn::Connection;
use selium_common::types::Operation;

/// The default `retention_policy` setting for messages.
pub const RETENTION_POLICY_DEFAULT: u64 = 0;

/// A convenient builder struct used to build a `Selium` stream, such as a
/// [Publisher](crate::Publisher) or [Subscriber](crate::Subscriber).
///
/// Similar to the [ClientBuilder](crate::ClientBuilder) struct, the [StreamBuilder] struct uses a
/// type-level Finite State Machine to assure that any stream instance cannot be constructed with an
/// invalid state. Using a [Publisher](crate::Publisher) stream as an example, the `open`
/// method will not be in-scope unless the [StreamBuilder](crate::StreamBuilder) is in a pre-open state, which
/// requires the stream to be configured, and a decoder to have been specified.
///
/// **NOTE:** The [StreamBuilder] type is not intended to be used directly, but rather, is
/// constructed via any of the methods on a [Client](crate::Client) instance. For example, the
/// [subscriber](crate::Client::subscriber) and [publisher](crate::Client::publisher) methods will
/// construct the respective StreamBuilder.
#[derive(Debug)]
pub struct StreamBuilder<T> {
    pub(crate) state: T,
    pub(crate) connection: Connection,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct StreamCommon {
    pub(crate) topic: String,
    pub(crate) retention_policy: u64,
    pub(crate) operations: Vec<Operation>,
}

impl StreamCommon {
    pub fn new(topic: &str) -> Self {
        Self {
            topic: topic.to_owned(),
            retention_policy: RETENTION_POLICY_DEFAULT,
            operations: Vec::new(),
        }
    }

    #[doc(hidden)]
    pub fn map(&mut self, module_path: &str) {
        self.operations.push(Operation::Map(module_path.into()));
    }

    #[doc(hidden)]
    pub fn filter(&mut self, module_path: &str) {
        self.operations.push(Operation::Filter(module_path.into()));
    }

    #[doc(hidden)]
    pub fn retain<T: TryIntoU64>(&mut self, policy: T) -> Result<()> {
        self.retention_policy = policy.try_into_u64()?;
        Ok(())
    }
}

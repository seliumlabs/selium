use crate::{constants::RETENTION_POLICY_DEFAULT, traits::TryIntoU64, Client};
use selium_protocol::Operation;
use selium_std::errors::Result;

/// A convenient builder struct used to build a `Selium` stream, such as a
/// [Pub/Sub](crate::streams::pubsub) or [Request/Reply](crate::streams::request_reply) stream.
///
/// Similar to the [ClientBuilder](crate::ClientBuilder) struct, the [StreamBuilder] struct uses a
/// type-level Finite State Machine to assure that any stream instance cannot be constructed with an
/// invalid state. Using a [Publisher](crate::streams::pubsub::Publisher) stream as an example, the `open`
/// method will not be in-scope unless the [StreamBuilder](crate::StreamBuilder) is in a pre-open state, which
/// requires the stream to be configured, and a decoder to have been specified.
///
/// **NOTE:** The [StreamBuilder] type is not intended to be used directly, but rather, is
/// constructed via any of the methods on a [Client](crate::Client) instance. For example, the
/// [subscriber](crate::Client::subscriber), [publisher](crate::Client::publisher),
/// [requestor](crate::Client::requestor) and [replier](crate::Client::replier) methods will
/// construct each respective StreamBuilder.
pub struct StreamBuilder<T> {
    pub(crate) state: T,
    pub(crate) client: Client,
}

impl<T> StreamBuilder<T> {
    pub fn new(client: Client, state: T) -> Self {
        Self { state, client }
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct PubSubCommon {
    pub(crate) topic: String,
    pub(crate) retention_policy: u64,
    pub(crate) operations: Vec<Operation>,
}

impl PubSubCommon {
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

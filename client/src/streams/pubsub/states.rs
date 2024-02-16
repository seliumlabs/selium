use crate::{
    batching::BatchConfig,
    streams::aliases::{Comp, Decomp},
    PubSubCommon,
};
use selium_protocol::Offset;

#[doc(hidden)]
pub struct SubscriberWantsDecoder {
    pub(crate) common: PubSubCommon,
}

impl SubscriberWantsDecoder {
    pub fn new(topic: &str) -> Self {
        Self {
            common: PubSubCommon::new(topic),
        }
    }
}

#[doc(hidden)]
pub struct SubscriberWantsOpen<D> {
    pub(crate) common: PubSubCommon,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) offset: Offset,
}

impl<D> SubscriberWantsOpen<D> {
    pub fn new(prev: SubscriberWantsDecoder, decoder: D) -> Self {
        Self {
            common: prev.common,
            decoder,
            decompression: None,
            offset: Offset::default(),
        }
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct PublisherWantsEncoder {
    pub(crate) common: PubSubCommon,
}

impl PublisherWantsEncoder {
    pub fn new(topic: &str) -> Self {
        Self {
            common: PubSubCommon::new(topic),
        }
    }
}

#[doc(hidden)]
pub struct PublisherWantsOpen<E> {
    pub(crate) common: PubSubCommon,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) batch_config: Option<BatchConfig>,
}

impl<E> PublisherWantsOpen<E> {
    pub fn new(prev: PublisherWantsEncoder, encoder: E) -> Self {
        Self {
            common: prev.common,
            encoder,
            compression: None,
            batch_config: None,
        }
    }
}

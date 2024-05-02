use crate::streams::aliases::{Comp, Decomp};
use std::{pin::Pin, time::Duration};

pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[doc(hidden)]
pub struct RequestorWantsRequestEncoder {
    pub(crate) endpoint: String,
}

impl RequestorWantsRequestEncoder {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
        }
    }
}

pub struct RequestorWantsReplyDecoder<E> {
    pub(crate) endpoint: String,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
}

impl<E> RequestorWantsReplyDecoder<E> {
    pub fn new(prev: RequestorWantsRequestEncoder, encoder: E) -> Self {
        Self {
            endpoint: prev.endpoint,
            encoder,
            compression: None,
        }
    }
}

pub struct RequestorWantsOpen<E, D> {
    pub(crate) endpoint: String,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) request_timeout: Duration,
}

impl<E, D> RequestorWantsOpen<E, D> {
    pub fn new(prev: RequestorWantsReplyDecoder<E>, decoder: D) -> Self {
        Self {
            endpoint: prev.endpoint,
            encoder: prev.encoder,
            compression: prev.compression,
            decoder,
            decompression: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

#[doc(hidden)]
pub struct ReplierWantsRequestDecoder {
    pub(crate) endpoint: String,
}

impl ReplierWantsRequestDecoder {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
        }
    }
}

#[doc(hidden)]
pub struct ReplierWantsReplyEncoder<D> {
    pub(crate) endpoint: String,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
}

impl<D> ReplierWantsReplyEncoder<D> {
    pub fn new(prev: ReplierWantsRequestDecoder, decoder: D) -> Self {
        Self {
            endpoint: prev.endpoint,
            decoder,
            decompression: None,
        }
    }
}

#[doc(hidden)]
pub struct ReplierWantsHandler<D, E> {
    pub(crate) endpoint: String,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
}

impl<D, E> ReplierWantsHandler<D, E> {
    pub fn new(prev: ReplierWantsReplyEncoder<D>, encoder: E) -> Self {
        Self {
            endpoint: prev.endpoint,
            decoder: prev.decoder,
            decompression: prev.decompression,
            encoder,
            compression: None,
        }
    }
}

#[doc(hidden)]
pub struct ReplierWantsOpen<D, E, F> {
    pub(crate) endpoint: String,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) handler: Pin<Box<F>>,
}

impl<D, E, F> ReplierWantsOpen<D, E, F> {
    pub fn new(prev: ReplierWantsHandler<D, E>, handler: F) -> Self {
        Self {
            endpoint: prev.endpoint,
            decoder: prev.decoder,
            decompression: prev.decompression,
            encoder: prev.encoder,
            compression: prev.compression,
            handler: Box::pin(handler),
        }
    }
}

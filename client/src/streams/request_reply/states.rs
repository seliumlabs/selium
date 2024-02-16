use crate::streams::aliases::{Comp, Decomp};
use std::{marker::PhantomData, pin::Pin, time::Duration};

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

pub struct RequestorWantsOpen<E, D, ResItem> {
    pub(crate) endpoint: String,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) request_timeout: Duration,
    _res_marker: PhantomData<ResItem>,
}

impl<E, D, ResItem> RequestorWantsOpen<E, D, ResItem> {
    pub fn new(prev: RequestorWantsReplyDecoder<E>, decoder: D) -> Self {
        Self {
            endpoint: prev.endpoint,
            encoder: prev.encoder,
            compression: prev.compression,
            decoder,
            decompression: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            _res_marker: PhantomData,
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
pub struct ReplierWantsReplyEncoder<D, ReqItem> {
    pub(crate) endpoint: String,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) _req_marker: PhantomData<ReqItem>,
}

impl<D, ReqItem> ReplierWantsReplyEncoder<D, ReqItem> {
    pub fn new(prev: ReplierWantsRequestDecoder, decoder: D) -> Self {
        Self {
            endpoint: prev.endpoint,
            decoder,
            decompression: None,
            _req_marker: PhantomData,
        }
    }
}

#[doc(hidden)]
pub struct ReplierWantsHandler<D, E, ReqItem> {
    pub(crate) endpoint: String,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) _req_marker: PhantomData<ReqItem>,
}

impl<D, E, ReqItem> ReplierWantsHandler<D, E, ReqItem> {
    pub fn new(prev: ReplierWantsReplyEncoder<D, ReqItem>, encoder: E) -> Self {
        Self {
            endpoint: prev.endpoint,
            decoder: prev.decoder,
            decompression: prev.decompression,
            encoder,
            compression: None,
            _req_marker: prev._req_marker,
        }
    }
}

#[doc(hidden)]
pub struct ReplierWantsOpen<D, E, F, ReqItem> {
    pub(crate) endpoint: String,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) handler: Pin<Box<F>>,
    pub(crate) _req_marker: PhantomData<ReqItem>,
}

impl<D, E, F, ReqItem> ReplierWantsOpen<D, E, F, ReqItem> {
    pub fn new(prev: ReplierWantsHandler<D, E, ReqItem>, handler: F) -> Self {
        Self {
            endpoint: prev.endpoint,
            decoder: prev.decoder,
            decompression: prev.decompression,
            encoder: prev.encoder,
            compression: prev.compression,
            handler: Box::pin(handler),
            _req_marker: prev._req_marker,
        }
    }
}

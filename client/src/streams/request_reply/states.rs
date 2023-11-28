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

pub struct RequestorWantsReplyDecoder<E, ReqItem> {
    pub(crate) endpoint: String,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    _req_marker: PhantomData<ReqItem>,
}

impl<E, ReqItem> RequestorWantsReplyDecoder<E, ReqItem> {
    pub fn new(prev: RequestorWantsRequestEncoder, encoder: E) -> Self {
        Self {
            endpoint: prev.endpoint,
            encoder,
            compression: None,
            _req_marker: PhantomData,
        }
    }
}

pub struct RequestorWantsOpen<E, D, ReqItem, ResItem> {
    pub(crate) endpoint: String,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) request_timeout: Duration,
    _req_marker: PhantomData<ReqItem>,
    _res_marker: PhantomData<ResItem>,
}

impl<E, D, ReqItem, ResItem> RequestorWantsOpen<E, D, ReqItem, ResItem> {
    pub fn new(prev: RequestorWantsReplyDecoder<E, ReqItem>, decoder: D) -> Self {
        Self {
            endpoint: prev.endpoint,
            encoder: prev.encoder,
            compression: prev.compression,
            decoder,
            decompression: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            _req_marker: prev._req_marker,
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
pub struct ReplierWantsHandler<D, E, ReqItem, ResItem> {
    pub(crate) endpoint: String,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) _req_marker: PhantomData<ReqItem>,
    pub(crate) _res_marker: PhantomData<ResItem>,
}

impl<D, E, ReqItem, ResItem> ReplierWantsHandler<D, E, ReqItem, ResItem> {
    pub fn new(prev: ReplierWantsReplyEncoder<D, ReqItem>, encoder: E) -> Self {
        Self {
            endpoint: prev.endpoint,
            decoder: prev.decoder,
            decompression: prev.decompression,
            encoder,
            compression: None,
            _req_marker: prev._req_marker,
            _res_marker: PhantomData,
        }
    }
}

#[doc(hidden)]
pub struct ReplierWantsOpen<D, E, F, ReqItem, ResItem> {
    pub(crate) endpoint: String,
    pub(crate) decoder: D,
    pub(crate) decompression: Option<Decomp>,
    pub(crate) encoder: E,
    pub(crate) compression: Option<Comp>,
    pub(crate) handler: Pin<Box<F>>,
    pub(crate) _req_marker: PhantomData<ReqItem>,
    pub(crate) _res_marker: PhantomData<ResItem>,
}

impl<D, E, F, ReqItem, ResItem> ReplierWantsOpen<D, E, F, ReqItem, ResItem> {
    pub fn new(prev: ReplierWantsHandler<D, E, ReqItem, ResItem>, handler: F) -> Self {
        Self {
            endpoint: prev.endpoint,
            decoder: prev.decoder,
            decompression: prev.decompression,
            encoder: prev.encoder,
            compression: prev.compression,
            handler: Box::pin(handler),
            _req_marker: prev._req_marker,
            _res_marker: prev._res_marker,
        }
    }
}

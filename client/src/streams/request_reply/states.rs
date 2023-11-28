use crate::streams::aliases::{Comp, Decomp};
use std::{marker::PhantomData, pin::Pin};

#[doc(hidden)]
pub struct ReplierWantsRequestDecoder {
    endpoint: String,
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

use std::pin::Pin;

use anyhow::{anyhow, Context, Result as AnyhowResult};
use bytes::BytesMut;
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    future, Sink, SinkExt, Stream, StreamExt,
};
use quinn::Connection;
use selium_protocol::{Frame, MessagePayload, TopicName};
use selium_proxy::{AdminRequest, AdminResponse};
use selium_std::{
    codecs::BincodeCodec,
    errors::{CodecError, Result, SeliumError},
    traits::codec::{MessageDecoder, MessageEncoder},
};

use crate::{
    quic::get_pubkey_from_connection,
    server::SharedTopics,
    topic::{reqrep, Socket},
};

// XXX This is horrendously inefficient! Caching is needed.
pub async fn do_cloud_auth(
    connection: &Connection,
    name: &TopicName,
    topics: &SharedTopics,
) -> AnyhowResult<()> {
    let mut ts = topics.lock().await;

    let proxy_namespace = TopicName::create("selium", "proxy").unwrap();

    let pub_key = get_pubkey_from_connection(&connection)?;
    let namespace = name.namespace();

    if ts.contains_key(&proxy_namespace) {
        let ((si, st), (mut tx, rx)) = channel_pair();

        let topic_tx = ts.get_mut(&proxy_namespace).unwrap();
        topic_tx
            .send(Socket::Reqrep(reqrep::Socket::Client((
                Box::pin(si.sink_map_err(|_| SeliumError::RequestFailed)),
                Box::pin(st),
            ))))
            .await
            .context("Failed to add Requestor to proxy topic")?;

        tx.send(AdminRequest::GetNamespace(pub_key)).await?;
        let (response, _) = rx.into_future().await;
        match response {
            Some(Ok(AdminResponse::GetNamespaceResponse(ns))) if ns == namespace => Ok(()),
            Some(Ok(AdminResponse::GetNamespaceResponse(_))) => Err(anyhow!("Access denied")),
            Some(Ok(_)) => Err(anyhow!("Invalid response from proxy")),
            Some(Err(e)) => Err(e.into()),
            None => Err(anyhow!("No response from proxy")),
        }
    } else {
        Err(anyhow!("Waiting for proxy - please retry"))
    }
}

fn channel_pair() -> (
    (UnboundedSender<Frame>, UnboundedReceiver<Result<Frame>>),
    (
        Pin<Box<dyn Sink<AdminRequest, Error = SeliumError> + Send>>,
        Pin<Box<dyn Stream<Item = Result<AdminResponse>> + Send>>,
    ),
) {
    let (si, rx) = unbounded();
    let (tx, st) = unbounded();

    let bincode = BincodeCodec::default();
    let tx = tx
        .sink_map_err(|_| SeliumError::RequestFailed)
        .with(move |item| match bincode.encode(item) {
            Ok(msg) => future::ok(Ok(Frame::Message(MessagePayload {
                headers: None,
                message: msg,
            }))),
            Err(e) => future::err(SeliumError::Codec(CodecError::EncodeFailure(e))),
        });

    let bincode = BincodeCodec::default();
    let rx = rx.map(move |frame| match frame {
        Frame::Message(payload) => {
            let mut bytes = BytesMut::new();
            bytes.extend(payload.message);
            match bincode.decode(&mut bytes) {
                Ok(item) => Ok(item),
                Err(e) => Err(SeliumError::Codec(CodecError::DecodeFailure(e))),
            }
        }
        _ => Err(SeliumError::RequestFailed),
    });

    ((si, st), (Box::pin(tx), Box::pin(rx)))
}

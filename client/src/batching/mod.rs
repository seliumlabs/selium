//! Data structures and utilities to enable message batching on streams.
//!
//! Message batching is an optimization that batches several messages into a single frame, to reduce network and
//! compression calls for particulaly chatty [Publisher](crate::Publisher) streams.
//!
//! # Publisher
//!
//! In `Selium`, message batching uses an algorithm that collects messages sent to the
//! [Publisher](crate::Publisher) stream's [Sink](futures::Sink) implementation by any means, and pushes
//! them to a batching queue.
//!
//! Batching is an opt-in functionality for [Publisher](crate::Publisher) streams. If you wish to enable
//! batching for your [Publisher](crate::Publisher) stream, you can do so via the `with_batching` method
//! on the Publisher [ClientBuilder](crate::ClientBuilder).
//!
//! ## Batching Algorithm
//!
//! The batching algorithm is tuned by providing a [BatchConfig] instance, which specifies the
//! `batching interval` and `maximum batch size`. The queue will continue to collect new messages until
//! either the batch size has been exceeded, or the batching interval has expired. Batches will then be
//! encoded [into a message frame](selium_protocol::Frame::BatchMessage) recognized by the wire protocol,
//! before applying compression (if specified) and sending it over the wire.
//!
//! If a batch is incomplete prior to closing a [Publisher](crate::Publisher) stream, calling
//! [finish](crate::Publisher::finish) on the stream will automatically flush the pending message
//! batch to ensure that it is delivered to subscribers.

//! # Subscriber
//!
//! No stream configuration is required to enable message batching for a
//! [Publisher](crate::Publisher) stream. As message batches are recieved over the wire in a
//! [Frame::BatchMessage](selium_protocol::Frame::BatchMessage) frame, the
//! [Stream](futures::Stream) implementation for the [Publisher](crate::Publisher) stream will
//! decompress the batch payload (if specified), and then unpack the batch and deliver each message
//! individually.

mod batch_config;
mod message_batch;

pub use batch_config::*;
pub(crate) use message_batch::*;

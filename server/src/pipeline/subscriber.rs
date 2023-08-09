use futures::{Sink, SinkExt as _, future};
use crate::sink::SinkExt;
use log::warn;
use parking_lot::Mutex;
use selium_common::protocol::Frame;
use selium_common::types::{BiStream, Operation};
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

#[derive(Clone)]
pub struct Subscriber {
    sink: Arc<Mutex<BiStream>>,
    operations: Vec<Operation>,
}

impl Subscriber {
    pub fn new(sink: BiStream, operations: Vec<Operation>) -> Self {
        Self {
            sink: Arc::new(Mutex::new(sink)),
            operations,
        }
    }
}

impl Sink<Frame> for Subscriber {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sink
            .lock()
            .poll_ready_unpin(cx)
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))
    }

    fn start_send(self: Pin<&mut Self>, item: Frame) -> io::Result<()> {
        println!("{item:?}");
        self.sink
            .lock()
            .start_send_unpin(item)
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sink
            .lock()
            .poll_flush_unpin(cx)
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sink
            .lock()
            .poll_close_unpin(cx)
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))
    }
}

// pub fn filter<P, Item, Si>(path: P, si: Si) -> impl Sink<Item>
// where
//     P: Into<PathBuf>,
//     Si: Sink<Item>,
// {
//     warn!("Filters are unimplemented: {:?}", path.into());
//     si.filter(|_item| future::ready(true))
// }
//
// pub fn map<P, Item, Si>(path: P, si: Si) -> impl Sink<Item>
// where
//     P: Into<PathBuf>,
//     Si: Sink<Item>,
// {
//     warn!("Maps are unimplemented: {:?}", path.into());
//     si.map(|item| item)
// }

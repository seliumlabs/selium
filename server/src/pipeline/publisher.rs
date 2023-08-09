use crate::Frame;
use log::warn;
use parking_lot::Mutex;
use futures::{Stream, StreamExt, future};
use selium_common::types::{Operation, BiStream};
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll}, path::PathBuf,
};

#[derive(Clone)]
pub struct Publisher {
    stream: Arc<Mutex<BiStream>>,
    operations: Vec<Operation>,
}

impl Publisher {
    pub fn new(stream: BiStream, operations: Vec<Operation>) -> Self {
        Self {
            stream: Arc::new(Mutex::new(stream)),
            operations,
        }
    }
}

impl Stream for Publisher {
    type Item = io::Result<Frame>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut stream = self.stream.lock();

        let message = match futures::ready!(stream.poll_next_unpin(cx)) {
            Some(Ok(msg)) => msg,
            Some(Err(err)) => {
                return Poll::Ready(Some(Err(io::Error::new(
                    io::ErrorKind::Other,
                    err.to_string(),
                ))))
            }
            None => return Poll::Ready(None),
        };


        // for op in self.operations {
        //     match op {
        //         Operation::Map(path) => map(path, stream),
        //         Operation::Filter(path) => filter(path, stream)
        //     };
        // }

        Poll::Ready(Some(Ok(message)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.stream.lock().size_hint()
    }
}

pub fn filter<P, St>(path: P, st: St) -> impl Stream<Item = St::Item>
where
    P: Into<PathBuf>,
    St: Stream,
{
    warn!("Filters are unimplemented: {:?}", path.into());
    st.filter(|_item| future::ready(true))
}

pub fn map<P, St>(path: P, st: St) -> impl Stream<Item = St::Item>
where
    P: Into<PathBuf>,
    St: Stream,
{
    warn!("Maps are unimplemented: {:?}", path.into());
    st.map(|item| item)
}

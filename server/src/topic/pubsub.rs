use crate::{sink::FanoutMany, BoxSink};
use futures::{
    channel::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
    ready,
    stream::BoxStream,
    Future, Sink, Stream,
};
use log::{debug, error};
use pin_project_lite::pin_project;
use selium_protocol::traits::{ShutdownSink, ShutdownStream};
use selium_protocol::{Frame};
use selium_std::errors::Result;
use std::collections::HashMap;
use std::{
    fmt::Debug,
    pin::Pin,
    task::{Context, Poll},
};
use tokio_stream::StreamMap;

const SOCK_CHANNEL_SIZE: usize = 100;

pub type TopicShutdown = oneshot::Receiver<()>;

pub enum Socket<T, E> {
    Publisher(BoxStream<'static, Result<T>>, BoxSink<Frame, E>),
    Sink(BoxSink<T, E>),
}

pin_project! {
    #[project = TopicProj]
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct Topic<T, E> {
        #[pin]
        stream: StreamMap<usize, BoxStream<'static, Result<T>>>,
        #[pin]
        result_map: HashMap<usize, BoxSink<Frame, E>>,
        next_stream_id: usize,
        #[pin]
        sink: FanoutMany<usize, BoxSink<T, E>>,
        next_sink_id: usize,
        #[pin]
        handle: Receiver<Socket<T, E>>,
        buffered_item: Option<T>,
    }
}

impl<T, E> Topic<T, E> {
    pub fn pair() -> (Self, Sender<Socket<T, E>>) {
        let (tx, rx) = mpsc::channel(SOCK_CHANNEL_SIZE);

        (
            Self {
                stream: StreamMap::new(),
                result_map: HashMap::new(),
                next_stream_id: 0,
                sink: FanoutMany::new(),
                next_sink_id: 0,
                handle: rx,
                buffered_item: None,
            },
            tx,
        )
    }
}

impl<T, E> Future for Topic<T, E>
where
    E: Debug + Unpin,
    T: Clone + Unpin,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let TopicProj {
            mut stream,
            mut result_map,
            next_stream_id,
            mut sink,
            next_sink_id,
            mut handle,
            buffered_item,
        } = self.project();

        loop {
            // If we've got an item buffered already, we need to write it to the sink
            // before we can do anything else.
            if buffered_item.is_some() {
                // Unwrapping is safe as the underlying sink is guaranteed not to error
                ready!(sink.as_mut().poll_ready(cx)).unwrap();
                let item = buffered_item.take().unwrap();
                sink.as_mut().start_send(item).unwrap();
            }

            match handle.as_mut().poll_next(cx) {
                Poll::Ready(Some(sock)) => match sock {
                    Socket::Publisher(st, si) => {
                        stream.as_mut().insert(*next_stream_id, st);
                        result_map.as_mut().insert(*next_stream_id, si);

                        *next_stream_id += 1;
                    }
                    Socket::Sink(si) => {
                        sink.as_mut().insert(*next_sink_id, si);
                        *next_sink_id += 1;
                    }
                },
                // If handle is terminated, the stream is dead
                Poll::Ready(None) => {
                    ready!(sink.as_mut().poll_flush(cx)).unwrap();
                    stream.iter_mut().for_each(|(_, s)| s.shutdown_stream());
                    sink.iter_mut().for_each(|(_, s)| s.shutdown_sink());

                    return Poll::Ready(());
                }
                // If no messages are available and there's no work to do, block this future
                Poll::Pending if stream.is_empty() && buffered_item.is_none() => {
                    return Poll::Pending
                }
                // Otherwise, move on with running the stream
                Poll::Pending => (),
            }

            match stream.as_mut().poll_next(cx) {
                // Received message from an inner stream
                Poll::Ready(Some((id, Ok(item)))) => {
                    *buffered_item = Some(item);

                    debug!("sending Frame ok...");
                    let sink = result_map.as_mut().get_mut().get_mut(&id).unwrap();
                    sink.as_mut().start_send(Frame::Ok).unwrap();
                    let _ = sink.as_mut().poll_flush(cx);
                }
                // Encountered an error whilst receiving a message from an inner stream
                Poll::Ready(Some((_, Err(e)))) => {
                    error!("Received invalid message from stream: {e:?}")
                }
                // All streams have finished
                // Unwrapping is safe as the underlying sink is guaranteed not to error
                Poll::Ready(None) => ready!(sink.as_mut().poll_flush(cx)).unwrap(),
                // No messages are available at this time
                Poll::Pending => {
                    // Unwrapping is safe as the underlying sink is guaranteed not to error
                    ready!(sink.poll_flush(cx)).unwrap();
                    return Poll::Pending;
                }
            }
        }
    }
}

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use anyhow::Result;
use futures::{
    channel::mpsc::{self, Receiver, Sender},
    ready, Future, Sink, SinkExt, Stream, StreamExt,
};
use pin_project_lite::pin_project;
use tokio_stream::StreamMap;

use crate::sink::FanoutMany;

pub struct BoxedStream<T>(pub Pin<Box<dyn Stream<Item = T> + Send + Sync>>);

pub struct BoxedSink<T, E>(pub Pin<Box<dyn Sink<T, Error = E> + Send + Sync>>);

pub enum Socket<St, Si, E> {
    Stream(BoxedStream<St>),
    Sink(BoxedSink<Si, E>),
}

pin_project! {
    #[project = TopicProj]
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct Topic<St, Si, E> {
        #[pin]
        stream: StreamMap<usize, BoxedStream<St>>,
        next_stream_id: usize,
        #[pin]
        sink: FanoutMany<usize, BoxedSink<Si, E>>,
        next_sink_id: usize,
        #[pin]
        handle: Receiver<Socket<St, Si, E>>,
        buffered_item: Option<Si>,
    }
}

impl<T> Stream for BoxedStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_next_unpin(cx)
    }
}

impl<T, E> Sink<T> for BoxedSink<T, E> {
    type Error = E;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.0.poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: T) -> std::result::Result<(), Self::Error> {
        self.0.start_send_unpin(item)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.0.poll_close_unpin(cx)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.0.poll_close_unpin(cx)
    }
}

impl<St, Si, E> Topic<St, Si, E> {
    pub fn pair() -> (Self, Sender<Socket<St, Si, E>>) {
        let (tx, rx) = mpsc::channel(10);

        (
            Self {
                stream: StreamMap::new(),
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

impl<St, Si, E> Future for Topic<St, Si, E>
where
    Si: Clone + Unpin,
{
    type Output = Result<(), E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let TopicProj {
            mut stream,
            next_stream_id,
            mut sink,
            next_sink_id,
            handle,
            buffered_item,
        } = self.project();

        loop {
            match handle.poll_next(cx) {
                Poll::Ready(Some(sock)) => match sock {
                    Socket::Stream(st) => {
                        stream.insert(*next_stream_id, st);
                        *next_stream_id += 1;
                    }
                    Socket::Sink(si) => {
                        sink.insert(*next_sink_id, si);
                        *next_sink_id += 1;
                    }
                },
                Poll::Ready(None) => return Poll::Ready(Ok(())), // if handle is terminated, the stream is dead
                Poll::Pending => (),
            }

            // If we've got an item buffered already, we need to write it to the
            // sink before we can do anything else
            if buffered_item.is_some() {
                ready!(sink.poll_ready(cx))?;
                sink.as_mut().start_send(buffered_item.take().unwrap())?;
            }

            match stream.poll_next(cx) {
                Poll::Ready(Some((_, item))) => {
                    *buffered_item = Some(item);
                }
                Poll::Ready(None) => {
                    ready!(sink.poll_close(cx))?;
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => {
                    ready!(sink.poll_flush(cx))?;
                    return Poll::Pending;
                }
            }
        }
    }
}

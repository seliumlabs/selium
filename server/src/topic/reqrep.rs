use crate::{sink::Router, BoxSink};
use futures::{
    channel::mpsc::{self, Receiver, Sender},
    ready,
    stream::BoxStream,
    Future, Sink, SinkExt, Stream, StreamExt,
};
use log::error;
use pin_project_lite::pin_project;
use selium_protocol::{
    traits::{ShutdownSink, ShutdownStream},
    Frame,
};
use selium_std::errors::Result;
use std::{
    collections::HashMap,
    fmt::Debug,
    pin::Pin,
    task::{Context, Poll},
};
use tokio_stream::StreamMap;

const SOCK_CHANNEL_SIZE: usize = 100;

pub enum Socket<E> {
    Client((BoxSink<Frame, E>, BoxStream<'static, Result<Frame>>)),
    Server((BoxSink<Frame, E>, BoxStream<'static, Result<Frame>>)),
}

pin_project! {
    #[project = TopicProj]
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct Topic<E> {
        #[pin]
        server: Option<(BoxSink<Frame, E>, BoxStream<'static, Result<Frame>>)>,
        #[pin]
        stream: StreamMap<usize, BoxStream<'static, Result<Frame>>>,
        #[pin]
        sink: Router<usize, BoxSink<Frame, E>>,
        next_id: usize,
        #[pin]
        handle: Receiver<Socket<E>>,
        buffered_req: Option<Frame>,
        buffered_rep: Option<Frame>,
    }
}

impl<E> Topic<E> {
    pub fn pair() -> (Self, Sender<Socket<E>>) {
        let (tx, rx) = mpsc::channel(SOCK_CHANNEL_SIZE);

        (
            Self {
                server: None,
                stream: StreamMap::new(),
                sink: Router::new(),
                next_id: 0,
                handle: rx,
                buffered_req: None,
                buffered_rep: None,
            },
            tx,
        )
    }
}

impl<E> Future for Topic<E>
where
    E: Debug + Unpin,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let TopicProj {
            mut server,
            mut stream,
            mut sink,
            next_id,
            mut handle,
            buffered_req,
            buffered_rep,
        } = self.project();

        loop {
            let mut server_pending = false;
            let mut stream_pending = false;

            // If we've got a request buffered already, we need to write it to the replier
            // before we can do anything else.
            if buffered_req.is_some() && server.is_some() {
                let si = &mut server.as_mut().as_pin_mut().unwrap().0;
                // Unwrapping is safe as the underlying sink is guaranteed not to error
                ready!(si.poll_ready_unpin(cx)).unwrap();
                si.start_send_unpin(buffered_req.take().unwrap()).unwrap();
            }

            match handle.as_mut().poll_next(cx) {
                Poll::Ready(Some(sock)) => match sock {
                    Socket::Client((si, st)) => {
                        stream.as_mut().insert(*next_id, st);
                        sink.as_mut().insert(*next_id, si);

                        *next_id += 1;
                    }
                    Socket::Server(bi) => {
                        if server.is_some() {
                            // XXX Return error to BiStream if server is Some
                        } else {
                            let _ = server.insert(bi);
                        }
                    }
                },
                // If handle is terminated, the stream is dead
                Poll::Ready(None) => {
                    ready!(sink.as_mut().poll_flush(cx)).unwrap();
                    stream.iter_mut().for_each(|(_, s)| s.shutdown_stream());
                    sink.iter_mut().for_each(|(_, s)| s.shutdown_sink());

                    if server.is_some() {
                        server.as_mut().as_pin_mut().unwrap().1.shutdown_stream();
                    }

                    return Poll::Ready(());
                }
                // If no messages are available and there's no work to do, block this future
                Poll::Pending
                    if stream.is_empty()
                        && server.is_none()
                        && buffered_req.is_none()
                        && buffered_rep.is_none() =>
                {
                    return Poll::Pending
                }
                // Otherwise, move on with running the stream
                Poll::Pending => (),
            }

            if server.is_some() {
                let st = &mut server.as_mut().as_pin_mut().unwrap().1;

                match st.poll_next_unpin(cx) {
                    // Received message from the server stream
                    Poll::Ready(Some(Ok(item))) => {
                        *buffered_rep = Some(item);
                    }
                    // Encountered an error whilst receiving a message from an inner stream
                    Poll::Ready(Some(Err(e))) => {
                        error!("Received invalid message from replier: {e:?}")
                    }
                    // Server has finished
                    Poll::Ready(None) => {
                        if server.is_some() {
                            let si = &mut server.as_mut().as_pin_mut().unwrap().0;
                            ready!(si.poll_flush_unpin(cx)).unwrap();
                        }

                        ready!(sink.as_mut().poll_flush(cx)).unwrap();
                        *server = None;
                    }
                    // No messages are available at this time
                    Poll::Pending => {
                        server_pending = true;
                    }
                }
            }

            // If we've got a reply buffered already, we need to write it to the sink
            // before we can do anything else.
            if buffered_rep.is_some() {
                // Unwrapping is safe as the underlying sink is guaranteed not to error
                ready!(sink.as_mut().poll_ready(cx)).unwrap();

                let r = sink.as_mut().start_send(buffered_rep.take().unwrap());

                if let Some(e) = r.err() {
                    error!("Failed to send reply to requestor: {e:?}");
                }
            }

            match stream.as_mut().poll_next(cx) {
                // Received message from a client stream
                Poll::Ready(Some((id, Ok(item)))) => {
                    let mut payload = item.unwrap_message();
                    payload
                        .headers
                        .get_or_insert(HashMap::new())
                        .insert("cid".into(), format!("{id}"));
                    *buffered_req = Some(Frame::Message(payload));
                }
                // Encountered an error whilst receiving a message from an inner stream
                Poll::Ready(Some((_, Err(e)))) => {
                    error!("Received invalid message from requestor: {e:?}")
                }
                // All streams have finished
                // Unwrapping is safe as the underlying sink is guaranteed not to error
                Poll::Ready(None) => {
                    ready!(sink.as_mut().poll_flush(cx)).unwrap();

                    if server.is_some() {
                        let si = &mut server.as_mut().as_pin_mut().unwrap().0;
                        ready!(si.poll_flush_unpin(cx)).unwrap();
                    }
                }
                // No messages are available at this time
                Poll::Pending => {
                    stream_pending = true;
                }
            }

            if server_pending && stream_pending {
                // Unwrapping is safe as the underlying sink is guaranteed not to error
                ready!(sink.poll_flush(cx)).unwrap();

                if server.is_some() {
                    let si = &mut server.as_mut().as_pin_mut().unwrap().0;
                    ready!(si.poll_flush_unpin(cx)).unwrap();
                }

                return Poll::Pending;
            }
        }
    }
}

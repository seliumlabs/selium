use crate::sink::Router;
use futures::{
    channel::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
    ready, Future, Sink, Stream,
};
use log::error;
use pin_project_lite::pin_project;
use selium_protocol::{
    traits::{ShutdownSink, ShutdownStream},
    BiStream, Frame, ReadHalf, WriteHalf,
};
use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
};
use tokio_stream::StreamMap;

const SOCK_CHANNEL_SIZE: usize = 100;

pub type TopicShutdown = oneshot::Receiver<()>;

pub enum Socket {
    Client(BiStream),
    Server(BiStream),
}

pin_project! {
    #[project = TopicProj]
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct Topic {
        #[pin]
        server: Option<BiStream>,
        #[pin]
        stream: StreamMap<usize, ReadHalf>,
        #[pin]
        sink: Router<WriteHalf>,
        next_id: usize,
        #[pin]
        handle: Receiver<Socket>,
        buffered_req: Option<Frame>,
        buffered_rep: Option<Frame>,
    }
}

impl Topic {
    pub fn pair() -> (Self, Sender<Socket>) {
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

impl Future for Topic {
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
                let mut s = server.as_mut().as_pin_mut().unwrap();
                // Unwrapping is safe as the underlying sink is guaranteed not to error
                ready!(s.as_mut().poll_ready(cx)).unwrap();
                s.start_send(buffered_req.take().unwrap()).unwrap();
            }

            match handle.as_mut().poll_next(cx) {
                Poll::Ready(Some(sock)) => match sock {
                    Socket::Client(bi) => {
                        let (si, st) = bi.split();
                        stream.as_mut().insert(*next_id, st);
                        sink.as_mut().insert(*next_id, si);

                        *next_id += 1;
                    }
                    Socket::Server(bi) => {
                        // XXX Return error to BiStream if server is Some
                        server.get_or_insert(bi);
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
                Poll::Pending
                    if stream.is_empty() && buffered_req.is_none() && buffered_rep.is_none() =>
                {
                    return Poll::Pending
                }
                // Otherwise, move on with running the stream
                Poll::Pending => (),
            }

            if server.is_some() {
                match server.as_mut().as_pin_mut().unwrap().poll_next(cx) {
                    // Received message from the server stream
                    Poll::Ready(Some(Ok(item))) => {
                        *buffered_rep = Some(item);
                    }
                    // Encountered an error whilst receiving a message from an inner stream
                    Poll::Ready(Some(Err(e))) => {
                        error!("Received invalid message from stream: {e:?}")
                    }
                    // Server has finished
                    Poll::Ready(None) => (),
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
                    error!("Received invalid message from stream: {e:?}")
                }
                // All streams have finished
                // Unwrapping is safe as the underlying sink is guaranteed not to error
                Poll::Ready(None) => {
                    ready!(sink.as_mut().poll_flush(cx)).unwrap();

                    if server.is_some() {
                        ready!(server.as_mut().as_pin_mut().unwrap().poll_flush(cx)).unwrap();
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
                    ready!(server.as_mut().as_pin_mut().unwrap().poll_flush(cx)).unwrap();
                }

                return Poll::Pending;
            }
        }
    }
}

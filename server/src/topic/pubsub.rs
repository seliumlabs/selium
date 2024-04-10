use crate::BoxSink;
use bytes::Bytes;
use futures::{
    channel::mpsc::{self, Receiver, Sender},
    ready,
    stream::{BoxStream, FuturesUnordered},
    Future, FutureExt, SinkExt, StreamExt,
};
use selium_log::{
    data::LogIterator,
    message::{Headers, Message, MessageSlice},
    message_log::MessageLog,
};
use selium_protocol::{BatchPayload, Frame, MessagePayload, Offset};
use selium_std::errors::{Result, SeliumError, TopicError};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Poll, Waker},
};
use tokio::sync::RwLock;
use tokio_stream::StreamMap;

pub type SharedLog = Arc<RwLock<MessageLog>>;
pub type ReadFut = Pin<Box<dyn Future<Output = Result<MessageSlice>> + Send>>;

const SOCK_CHANNEL_SIZE: usize = 100;

pub enum Socket {
    Stream(BoxStream<'static, Result<Frame>>),
    Sink(BoxSink<Frame, SeliumError>, Offset),
}

pub struct Subscriber {
    offset: u64,
    log: SharedLog,
    sink: BoxSink<Frame, SeliumError>,
    buffered_slice: Option<LogIterator>,
    read_fut: ReadFut,
    waker: Option<Waker>,
}

impl Subscriber {
    pub fn new(offset: Offset, log: SharedLog, sink: BoxSink<Frame, SeliumError>) -> Self {
        Self {
            offset: 0,
            log: log.clone(),
            sink,
            buffered_slice: None,
            read_fut: build_read_future(log, 0),
            waker: None,
        }
    }

    pub fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake_by_ref();
        }
    }

    async fn read_messages(&mut self) {
    if let Some(slice) = self.buffered_slice {
        let next = slice.next().await.unwrap();
                let batch_size = message.headers().batch_size();
                let records = Bytes::copy_from_slice(message.records());

                let frame = if batch_size > 1 {
                    Frame::BatchMessage(BatchPayload {
                        message: records,
                        size: batch_size,
                    })
                } else {
                    Frame::Message(MessagePayload {
                        headers: None,
                        message: records,
                    })
                };

                self.sink.send(frame).await;
    }

    }
}

impl Future for Subscriber {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.waker = Some(cx.waker().clone());

        ready!(self.read_messages().poll_unpin(cx));

        if let Ok(slice) = ready!(self.read_fut.poll_unpin(cx)) {
            self.offset = slice.end_offset();
            self.buffered_slice = slice.messages();
            self.read_fut = build_read_future(self.log.clone(), self.offset);

            if self.buffered_slice.is_none() {
                return Poll::Pending;
            }
        }
    }
}

pub enum SubscribersEvent {
    NewSubscriber(Pin<Box<Subscriber>>),
    Wake,
}

pub struct Subscribers {
    notify: Receiver<SubscribersEvent>,
    futures: FuturesUnordered<Pin<Box<Subscriber>>>,
}

impl Subscribers {
    pub fn new() -> (Sender<SubscribersEvent>, Self) {
        let (tx, notify) = mpsc::channel(SOCK_CHANNEL_SIZE);
        let futures = FuturesUnordered::new();
        let subscribers = Self { notify, futures };
        (tx, subscribers)
    }

    fn wake_subscribers(&mut self) {
        self.futures.iter_mut().for_each(|f| f.wake());
    }
}

impl Future for Subscribers {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.notify.poll_next_unpin(cx) {
                Poll::Ready(Some(event)) => {
                    match event {
                        SubscribersEvent::Wake => self.wake_subscribers(),
                        SubscribersEvent::NewSubscriber(sub) => self.futures.push(sub),
                    };
                    cx.waker().wake_by_ref();
                }
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => (),
            }

            if self.futures.is_empty() {
                return Poll::Pending;
            } else {
                ready!(self.futures.poll_next_unpin(cx));
            }
        }
    }
}

pub struct Topic {
    publishers: StreamMap<usize, BoxStream<'static, Result<Frame>>>,
    next_stream_id: usize,
    notify: Sender<SubscribersEvent>,
    handle: Receiver<Socket>,
    log: SharedLog,
}

impl Topic {
    pub fn pair(log: SharedLog) -> (Self, Sender<Socket>) {
        let (tx, rx) = mpsc::channel(SOCK_CHANNEL_SIZE);
        let publishers = StreamMap::new();
        let (notify, subscribers) = Subscribers::new();
        tokio::spawn(subscribers);

        (
            Self {
                log,
                publishers,
                notify,
                next_stream_id: 0,
                handle: rx,
            },
            tx,
        )
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            tokio::select! {
                Some((_, Ok(frame))) = self.publishers.next() => {
                    let batch_size = frame.unwrap_batch_size();
                    let payload = frame.unwrap_payload();

                    let headers = Headers::new(payload.len(), batch_size, 1);
                    let message = Message::new(headers, &payload);
                    let mut log = self.log.write().await;

                    log.write(message).await?;
                    self.notify.send(SubscribersEvent::Wake).await
                        .map_err(TopicError::NotifySubscribers)?;
                },
                Some(socket) = self.handle.next() => match socket {
                    Socket::Stream(st) => {
                        self.publishers.insert(self.next_stream_id, st);
                        self.next_stream_id += 1;
                    }
                    Socket::Sink(si, offset) => {
                        let subscriber = Box::pin(Subscriber::new(offset, self.log.clone(), si));

                        self.notify
                            .send(SubscribersEvent::NewSubscriber(subscriber))
                            .await.map_err(TopicError::NotifySubscribers)?;
                    }
                }
            }
        }
    }
}

fn build_read_future(log: SharedLog, offset: u64) -> ReadFut {
    Box::pin({
        let log = log.clone();
        let range = offset..offset + 1000;

        async move {
            let log = log.read().await;
            let slice = log.read_slice(range).await.map_err(SeliumError::Log);
            return slice;
        }
    })
})

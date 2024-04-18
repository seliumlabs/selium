use crate::BoxSink;
use bytes::Bytes;
use futures::{
    channel::mpsc::{self, Receiver, Sender},
    stream::BoxStream,
    Future, SinkExt, StreamExt,
};
use selium_log::{
    data::LogIterator,
    message::{Headers, Message, MessageSlice},
    MessageLog,
};
use selium_protocol::{BatchPayload, Frame, MessagePayload, Offset};
use selium_std::errors::{Result, SeliumError, TopicError};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::{select, sync::RwLock};
use tokio_stream::StreamMap;
use tokio_util::sync::CancellationToken;

pub type SharedLog = Arc<RwLock<MessageLog>>;
pub type ReadFut = Pin<Box<dyn Future<Output = Result<MessageSlice>> + Send>>;
pub type SleepFut = Pin<Box<dyn Future<Output = ()> + Send>>;

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
}

impl Subscriber {
    pub fn new(offset: u64, log: SharedLog, sink: BoxSink<Frame, SeliumError>) -> Self {
        Self {
            offset,
            log: log.clone(),
            sink,
            buffered_slice: None,
        }
    }

    async fn read_messages(&mut self) {
        if let Some(slice) = self.buffered_slice.as_mut() {
            while let Some(Ok(message)) = slice.next().await {
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

                let _ = self.sink.send(frame).await;
            }
        }
    }

    async fn poll_for_messages(&mut self) {
        println!("Polling with offset: {}", self.offset);
        let slice = self
            .log
            .read()
            .await
            .read_slice(self.offset, None)
            .await
            .map_err(SeliumError::Log)
            .unwrap();

        self.offset = slice.end_offset();
        self.buffered_slice = slice.messages();

        if self.buffered_slice.is_some() {
            self.read_messages().await;
        } else {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

pub struct Subscribers {
    notify: Receiver<Pin<Box<Subscriber>>>,
    token: CancellationToken,
}

impl Subscribers {
    pub fn new() -> (Sender<Pin<Box<Subscriber>>>, Self) {
        let (tx, notify) = mpsc::channel(SOCK_CHANNEL_SIZE);
        let token = CancellationToken::new();
        let subscribers = Self { notify, token };
        (tx, subscribers)
    }

    pub async fn run(&mut self) {
        while let Some(mut subscriber) = self.notify.next().await {
            let token = self.token.clone();

            tokio::spawn(async move {
                loop {
                    select! {
                        _ = token.cancelled() => {
                            break;
                        },
                        _ = subscriber.poll_for_messages() => {
                            continue;
                        }
                    }
                }
            });
        }
    }
}

pub struct Topic {
    publishers: StreamMap<usize, BoxStream<'static, Result<Frame>>>,
    next_stream_id: usize,
    notify: Sender<Pin<Box<Subscriber>>>,
    handle: Receiver<Socket>,
    log: SharedLog,
}

impl Topic {
    pub fn pair(log: MessageLog) -> (Self, Sender<Socket>) {
        let log = Arc::new(RwLock::new(log));
        let (tx, rx) = mpsc::channel(SOCK_CHANNEL_SIZE);
        let publishers = StreamMap::new();
        let (notify, mut subscribers) = Subscribers::new();
        tokio::spawn(async move { subscribers.run().await });

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
                },
                Some(socket) = self.handle.next() => match socket {
                    Socket::Stream(st) => {
                        self.publishers.insert(self.next_stream_id, st);
                        self.next_stream_id += 1;
                    }
                    Socket::Sink(si, offset) => {
                        let entries = self.log.read().await.number_of_entries();

                        let log_offset = match offset {
                            Offset::FromBeginning(offset) => offset,
                            Offset::FromEnd(offset) => entries - offset
                        };

                        let subscriber = Box::pin(Subscriber::new(log_offset, self.log.clone(), si));

                        self.notify
                            .send(subscriber)
                            .await
                            .map_err(TopicError::NotifySubscribers)?;
                    }
                }
            }
        }
    }
}

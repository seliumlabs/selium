use crate::protocol::MessageCodec;
use quinn::{RecvStream, SendStream};
use tokio_util::codec::{FramedRead, FramedWrite};

pub type ReadStream = FramedRead<RecvStream, MessageCodec>;
pub type WriteStream = FramedWrite<SendStream, MessageCodec>;
pub type Streams = (WriteStream, ReadStream);

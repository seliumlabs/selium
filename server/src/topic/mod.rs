use anyhow::Result;
use futures::{channel::mpsc, SinkExt};

pub mod pubsub;
pub mod reqrep;

pub enum Socket {
    Pubsub(pubsub::Socket),
    Reqrep(reqrep::Socket),
}

impl Socket {
    fn unwrap_pubsub(self) -> pubsub::Socket {
        match self {
            Self::Pubsub(s) => s,
            _ => panic!("Attempted to unwrap non-pubsub socket"),
        }
    }

    fn unwrap_reqrep(self) -> reqrep::Socket {
        match self {
            Self::Reqrep(s) => s,
            _ => panic!("Attempted to unwrap non-reqrep socket"),
        }
    }
}

pub enum Sender {
    Pubsub(mpsc::Sender<pubsub::Socket>),
    ReqRep(mpsc::Sender<reqrep::Socket>),
}

impl Sender {
    pub async fn send(&mut self, sock: Socket) -> Result<()> {
        match self {
            Self::Pubsub(ref mut s) => s.send(sock.unwrap_pubsub()).await?,
            Self::ReqRep(ref mut s) => s.send(sock.unwrap_reqrep()).await?,
        }

        Ok(())
    }

    pub fn close_channel(&mut self) {
        match self {
            Self::Pubsub(ref mut s) => s.close_channel(),
            Self::ReqRep(ref mut s) => s.close_channel(),
        }
    }
}

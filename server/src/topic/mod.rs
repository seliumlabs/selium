use anyhow::Result;
use futures::{channel::mpsc, SinkExt};

pub mod pubsub;
pub mod reqrep;

pub enum Socket<St, Si> {
    Pubsub(pubsub::Socket<St, Si>),
    Reqrep(reqrep::Socket<St, Si>),
}

impl<St, Si> Socket<St, Si> {
    fn unwrap_pubsub(self) -> pubsub::Socket<St, Si> {
        match self {
            Self::Pubsub(s) => s,
            _ => panic!("Attempted to unwrap non-pubsub socket"),
        }
    }

    fn unwrap_reqrep(self) -> reqrep::Socket<St, Si> {
        match self {
            Self::Reqrep(s) => s,
            _ => panic!("Attempted to unwrap non-reqrep socket"),
        }
    }
}

pub enum Sender<St, Si> {
    Pubsub(mpsc::Sender<pubsub::Socket<St, Si>>),
    ReqRep(mpsc::Sender<reqrep::Socket<St, Si>>),
}

impl<St, Si> Sender<St, Si> {
    pub async fn send(&mut self, sock: Socket<St, Si>) -> Result<()> {
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

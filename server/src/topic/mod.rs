use anyhow::Result;
use futures::{channel::mpsc, SinkExt};

pub mod pubsub;
pub mod reqrep;

pub enum Socket<T, E> {
    Pubsub(pubsub::Socket<T, E>),
    Reqrep(reqrep::Socket<E>),
}

impl<T, E> Socket<T, E> {
    fn unwrap_pubsub(self) -> pubsub::Socket<T, E> {
        match self {
            Self::Pubsub(s) => s,
            _ => panic!("Attempted to unwrap non-pubsub socket"),
        }
    }

    fn unwrap_reqrep(self) -> reqrep::Socket<E> {
        match self {
            Self::Reqrep(s) => s,
            _ => panic!("Attempted to unwrap non-reqrep socket"),
        }
    }
}

pub enum Sender<T, E> {
    Pubsub(mpsc::Sender<pubsub::Socket<T, E>>),
    ReqRep(mpsc::Sender<reqrep::Socket<E>>),
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

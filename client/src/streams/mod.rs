mod aliases;
mod builder;

pub mod pubsub;
pub mod request_reply;
pub use builder::*;
use futures::StreamExt;
use selium_protocol::{BiStream, Frame};
use selium_std::errors::{Result, SeliumError};

// Handle response from Selium server on opening a stream
async fn handle_reply(stream: &mut BiStream) -> Result<()> {
    match stream.next().await {
        Some(Ok(Frame::Ok)) => Ok(()),
        Some(Ok(Frame::Error(bytes))) => match String::from_utf8(bytes.to_vec()) {
            Ok(s) => Err(SeliumError::OpenStream(s)),
            Err(_) => Err(SeliumError::OpenStream("Invalid UTF-8 error".into())),
        },
        Some(Ok(_)) => Err(SeliumError::OpenStream(
            "Invalid frame returned from server".into(),
        )),
        Some(Err(e)) => Err(e),
        None => Err(SeliumError::OpenStream("Stream closed prematurely".into())),
    }
}

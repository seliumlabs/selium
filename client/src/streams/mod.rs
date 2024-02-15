mod aliases;
mod builder;

pub mod pubsub;
pub mod request_reply;
pub use builder::*;
use futures::StreamExt;
use selium_protocol::{
    error_codes::{STREAM_CLOSED_PREMATURELY, UNKNOWN_ERROR},
    BiStream, Frame,
};
use selium_std::errors::{Result, SeliumError};

// Handle response from Selium server on opening a stream
async fn handle_reply(stream: &mut BiStream) -> Result<()> {
    match stream.next().await {
        Some(Ok(Frame::Ok)) => Ok(()),
        Some(Ok(Frame::Error(payload))) => match String::from_utf8(payload.message.to_vec()) {
            Ok(s) => Err(SeliumError::OpenStream(payload.code, s)),
            Err(_) => Err(SeliumError::OpenStream(
                payload.code,
                "Invalid UTF-8 error".into(),
            )),
        },
        Some(Ok(_)) => Err(SeliumError::OpenStream(
            UNKNOWN_ERROR,
            "Invalid frame returned from server".into(),
        )),
        Some(Err(e)) => Err(e),
        None => Err(SeliumError::OpenStream(
            STREAM_CLOSED_PREMATURELY,
            "Stream closed prematurely".into(),
        )),
    }
}

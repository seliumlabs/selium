use anyhow::Result;
use selium_log::{config::LogConfig, message::Message, MessageLog};
use std::sync::Arc;

const MESSAGE_VERSION: u32 = 1;

#[tokio::main]
async fn main() -> Result<()> {
    let config = LogConfig::from_path("path/to/segments/dir");
    let log = MessageLog::open(Arc::new(config)).await?;
    let message = Message::single(b"Hello, world!", MESSAGE_VERSION);

    log.write(message).await?;
    log.flush().await?;
    let slice = log.read_slice(0, None).await?;

    if let Some(mut iter) = slice.messages() {
        let next = iter.next().await?;
        println!("{next:?}")
    }

    Ok(())
}

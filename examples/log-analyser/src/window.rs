//! Windowed stream utilities.

use std::time::Duration;

use futures::{FutureExt, Stream, StreamExt, stream};
use selium_userland::time;
use tracing::warn;

/// Adds time-windowed collection to streams.
pub(crate) trait WindowExt: Stream + Sized {
    /// Collects items for each time window of `duration`.
    fn window(self, duration: Duration) -> impl Stream<Item = Vec<Self::Item>>
    where
        Self: Unpin,
    {
        windowed(self, duration)
    }
}

impl<S> WindowExt for S where S: Stream + Sized {}

/// Collects items from `stream` for each window of `duration`.
pub(crate) fn windowed<T, S>(stream: S, duration: Duration) -> impl Stream<Item = Vec<T>>
where
    S: Stream<Item = T> + Unpin,
{
    stream::unfold(Some(stream), move |state| async move {
        let mut stream = match state {
            Some(stream) => stream,
            None => return None,
        };

        let mut buffer = Vec::new();
        let sleep = time::sleep(duration).fuse();
        futures::pin_mut!(sleep);

        loop {
            let next_item = stream.next().fuse();
            futures::pin_mut!(next_item);

            futures::select! {
                item = next_item => {
                    match item {
                        Some(item) => buffer.push(item),
                        None => {
                            if buffer.is_empty() {
                                return None;
                            }
                            return Some((buffer, None));
                        }
                    }
                }
                sleep_result = sleep => {
                    if let Err(err) = sleep_result {
                        warn!(?err, "window sleep failed");
                        if buffer.is_empty() {
                            return None;
                        }
                        return Some((buffer, None));
                    }

                    return Some((buffer, Some(stream)));
                }
            }
        }
    })
}

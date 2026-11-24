//! FramedWrite must not acknowledge a new payload by completing an older
//! interrupted flush.

use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use selium_memory::FrameHeader;
use selium_wire::{FramedWrite, MessageTransport, error::Result};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

struct TransportState {
    written: Arc<Mutex<Vec<u8>>>,
    flushes: Arc<AtomicUsize>,
    pend_first_flush: bool,
    fail_second_flush: bool,
}

impl AsyncRead for TransportState {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}

impl AsyncWrite for TransportState {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.written.lock().expect("written").extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let number = self.flushes.fetch_add(1, Ordering::SeqCst) + 1;
        if number == 1 && self.pend_first_flush {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        if number == 2 && self.fail_second_flush {
            return Poll::Ready(Err(std::io::Error::other("injected flush failure")));
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl MessageTransport for TransportState {
    type Error = std::io::Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<bool>> {
        Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<bool>> {
        Poll::Ready(Ok(false))
    }

    fn generation(&self) -> Result<u64> {
        Ok(0)
    }
}

fn transport(pend_first_flush: bool, fail_second_flush: bool) -> TransportState {
    TransportState {
        written: Arc::new(Mutex::new(Vec::new())),
        flushes: Arc::new(AtomicUsize::new(0)),
        pend_first_flush,
        fail_second_flush,
    }
}

fn poll_once<F: std::future::Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    future.poll(&mut context)
}

fn frames(written: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut pos = 0;
    while pos < written.len() {
        let header =
            FrameHeader::decode(written.get(pos..).expect("header offset")).expect("header");
        let end = pos + FrameHeader::ENCODED_SIZE + header.len as usize;
        frames.push((
            header.tag,
            written
                .get(pos + FrameHeader::ENCODED_SIZE..end)
                .expect("payload slice")
                .to_vec(),
        ));
        pos = end;
    }
    frames
}

// scenario: framed-io/framed-write-acknowledges-its-own-payload/new-payload-follows-interrupted-write
#[tokio::test]
async fn cancelled_flush_does_not_acknowledge_a_new_payload() {
    let state = transport(true, false);
    let written = Arc::clone(&state.written);
    let mut framed = FramedWrite::new(state);

    // First write buffers "old" and parks in the interrupted flush.
    let mut first = Box::pin(framed.write_frame_with_flags_async("old".as_bytes(), 1, 0));
    assert!(poll_once(first.as_mut()).is_pending());
    drop(first);

    // A different payload must not be acknowledged merely because the older
    // flush completed.
    let mut second = Box::pin(framed.write_frame_with_flags_async("new".as_bytes(), 2, 0));
    match poll_once(second.as_mut()) {
        Poll::Ready(Ok(())) => {}
        other => panic!("expected the replacement payload to be sent, got {other:?}"),
    }

    let frames = frames(&written.lock().expect("written"));
    assert_eq!(
        frames,
        vec![(1, b"old".to_vec()), (2, b"new".to_vec())],
        "both payloads must be delivered, and the new one only after its own send"
    );
}

#[tokio::test]
async fn retrying_the_same_payload_does_not_duplicate_it() {
    let state = transport(true, false);
    let written = Arc::clone(&state.written);
    let mut framed = FramedWrite::new(state);

    let mut first = Box::pin(framed.write_frame_with_flags_async("same".as_bytes(), 1, 0));
    assert!(poll_once(first.as_mut()).is_pending());
    drop(first);

    let mut retry = Box::pin(framed.write_frame_with_flags_async("same".as_bytes(), 1, 0));
    match poll_once(retry.as_mut()) {
        Poll::Ready(Ok(())) => {}
        other => panic!("expected the retry to complete the interrupted write, got {other:?}"),
    }
    assert_eq!(
        frames(&written.lock().expect("written")),
        vec![(1, b"same".to_vec())],
        "retrying the same payload must not publish it twice"
    );
}

// scenario: framed-io/framed-write-acknowledges-its-own-payload/flush-fails-after-interruption
#[tokio::test]
async fn failed_older_flush_is_reported_and_the_new_payload_is_not_sent() {
    let state = transport(true, true);
    let written = Arc::clone(&state.written);
    let mut framed = FramedWrite::new(state);

    let mut first = Box::pin(framed.write_frame_with_flags_async("old".as_bytes(), 1, 0));
    assert!(poll_once(first.as_mut()).is_pending());
    drop(first);

    let mut second = Box::pin(framed.write_frame_with_flags_async("new".as_bytes(), 2, 0));
    assert!(matches!(poll_once(second.as_mut()), Poll::Ready(Err(_))));

    let frames = frames(&written.lock().expect("written"));
    assert_eq!(frames, vec![(1, b"old".to_vec())]);
}

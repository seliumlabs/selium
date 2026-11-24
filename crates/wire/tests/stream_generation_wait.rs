//! Streaming reads must not lose a frame committed during wait registration.

use std::{
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use futures::Stream;
use selium_memory::FrameHeader;
use selium_service::FlatMsg;
use selium_wire::{
    FramedRead, FramedWrite, MessageTransport, RpcConnection, RpcServerStreamClient, error::Result,
};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static PEER: Mutex<Option<DuplexStream>> = Mutex::new(None);

struct TestTransport(DuplexStream);

impl AsyncRead for TestTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for TestTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl MessageTransport for TestTransport {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<bool>> {
        Poll::Ready(Ok(true))
    }

    fn poll_peer_closed(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<bool>> {
        Poll::Ready(Ok(false))
    }

    fn generation(&self) -> Result<u64> {
        Ok(GENERATION.load(Ordering::SeqCst))
    }

    fn region_id(&self) -> u64 {
        42
    }
}

fn commit_during_registration(region_id: u64, observed_generation: u64, _waker: &Waker) {
    assert_eq!(region_id, 42);
    assert_eq!(observed_generation, 0);
    let payload = FlatMsg::encode(&"ready".to_string());
    let header = FrameHeader {
        len: payload.len() as u32,
        tag: 1,
        flags: FrameHeader::FLAG_READY | FrameHeader::FLAG_STREAM_ITEM,
    };
    let mut frame = header.encode().to_vec();
    frame.extend_from_slice(&payload);
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut peer = PEER.lock().expect("peer lock");
    let written =
        Pin::new(peer.as_mut().expect("reply peer installed")).poll_write(&mut cx, &frame);
    assert!(matches!(written, Poll::Ready(Ok(len)) if len == frame.len()));
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn ignore_generation_advance(_region_id: u64, _new_generation: u64) {}

struct CountWaker(AtomicUsize);

impl Wake for CountWaker {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn frames_committed_during_wait_registration_rewake_rpc_readers() {
    selium_memory::install_generation_callbacks(
        commit_during_registration,
        ignore_generation_advance,
    );

    let (request, _request_peer) = tokio::io::duplex(1024);
    let (reply, reply_peer) = tokio::io::duplex(1024);
    *PEER.lock().expect("peer lock") = Some(reply_peer);
    let mut client = RpcServerStreamClient::<String, String, TestTransport>::new(
        FramedWrite::new(TestTransport(request)),
        FramedRead::new(TestTransport(reply)),
    );
    let mut stream = client.call("start".to_string()).await.expect("call");

    let count = Arc::new(CountWaker(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&count));
    let mut cx = Context::from_waker(&waker);
    assert!(
        Pin::new(&mut stream).poll_next(&mut cx).is_pending(),
        "the first poll finds no frame before registration"
    );
    assert_eq!(
        count.0.load(Ordering::SeqCst),
        1,
        "the frame committed by registration must re-arm the task"
    );

    let item = Pin::new(&mut stream)
        .poll_next(&mut cx)
        .map(|item| item.expect("item present").expect("decode item"));
    assert_eq!(item, Poll::Ready("ready".to_string()));
    drop(stream);
    drop(client);

    // The unary server receive loop uses the shared generation-wait helper.
    // Exercise the same commit-during-registration race through its public
    // future, rather than only through server-streaming reply reads.
    GENERATION.store(0, Ordering::SeqCst);
    let (request, request_peer) = tokio::io::duplex(1024);
    *PEER.lock().expect("peer lock") = Some(request_peer);
    let (reply, _reply_peer) = tokio::io::duplex(1024);
    let mut connection = RpcConnection::<String, String, TestTransport>::new(
        FramedRead::new(TestTransport(request)),
        FramedWrite::new(TestTransport(reply)),
        0,
    );
    let mut receive = Box::pin(connection.recv());
    count.0.store(0, Ordering::SeqCst);
    assert!(
        receive.as_mut().poll(&mut cx).is_pending(),
        "the server finds no request before registration"
    );
    assert_eq!(
        count.0.load(Ordering::SeqCst),
        1,
        "the request committed by registration must re-arm the server"
    );
    let result = receive.as_mut().poll(&mut cx);
    assert!(
        matches!(&result, Poll::Ready(Ok(_))),
        "the buffered request must be returned on the next poll"
    );
    let Poll::Ready(Ok(request)) = result else {
        return;
    };
    assert_eq!(request.payload().expect("decode request"), "ready");
}

//! RPC session lifecycle: repeated create/cancel/drop must leave no region
//! behind.
//!
//! Runs in its own integration binary so the process-global heap region
//! provider is not shared with unrelated tests.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use selium_memory::{HeapRegionProvider, region_provider, set_region_provider};
use selium_service::FlatMsg;
use selium_shm::{
    ShmRendezvous,
    rpc::{self, RpcConnection, ServerStreamConnection},
};
use selium_wire::rpc::{IncomingConnection, Rendezvous};

#[derive(Debug, Clone, PartialEq)]
struct Ping(Vec<u8>);

#[derive(Debug, Clone, PartialEq)]
struct Pong(Vec<u8>);

impl FlatMsg for Ping {
    fn encode(value: &Self) -> Vec<u8> {
        value.0.clone()
    }

    fn decode(bytes: &[u8]) -> std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
        Ok(Self(bytes.to_vec()))
    }
}

impl FlatMsg for Pong {
    fn encode(value: &Self) -> Vec<u8> {
        value.0.clone()
    }

    fn decode(bytes: &[u8]) -> std::result::Result<Self, flatbuffers::InvalidFlatbuffer> {
        Ok(Self(bytes.to_vec()))
    }
}

struct PendingRendezvous {
    sent: Arc<Mutex<Option<u64>>>,
    complete_send: bool,
}

impl Rendezvous for PendingRendezvous {
    async fn send(&self, shared_id: u64) -> selium_wire::error::Result<()> {
        *self
            .sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(shared_id);
        if self.complete_send {
            return Ok(());
        }
        std::future::pending().await
    }

    async fn recv(&self) -> selium_wire::error::Result<IncomingConnection> {
        std::future::pending().await
    }
}

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    future.poll(&mut context)
}

fn attach_fails(shared_id: u64) -> bool {
    rpc::accept::<Ping, Pong>(IncomingConnection {
        client_process_id: 1,
        shared_id,
    })
    .is_err()
}

fn live_regions() -> usize {
    region_provider()
        .expect("region provider")
        .live_region_count()
        .expect("heap provider counts regions")
}

// scenario: connector-channel-forwarding/per-connection-region-lifecycle/reclamation-on-serving-guest-close
#[tokio::test]
async fn repeated_session_create_cancel_and_drop_keeps_regions_bounded() {
    drop(set_region_provider(Box::new(HeapRegionProvider::new())));
    let baseline = live_regions();

    for _ in 0..100 {
        // Unary: cancel while the rendezvous send is in flight.
        let sent = Arc::new(Mutex::new(None));
        {
            let mut connect = Box::pin(rpc::connect::<Ping, Pong, _>(
                PendingRendezvous {
                    sent: Arc::clone(&sent),
                    complete_send: false,
                },
                4096,
                4096,
            ));
            assert!(poll_once(connect.as_mut()).is_pending());
            let shared_id = sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("offered session id");
            drop(connect);
            assert!(
                attach_fails(shared_id),
                "cancelled send must reclaim its region"
            );
        }

        // Unary: cancel while waiting for server acceptance.
        let sent = Arc::new(Mutex::new(None));
        {
            let mut connect = Box::pin(rpc::connect::<Ping, Pong, _>(
                PendingRendezvous {
                    sent: Arc::clone(&sent),
                    complete_send: true,
                },
                4096,
                4096,
            ));
            assert!(poll_once(connect.as_mut()).is_pending());
            let shared_id = sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("offered session id");
            drop(connect);
            assert!(
                attach_fails(shared_id),
                "cancelled acceptance must reclaim its region"
            );
        }

        // Streaming: cancel while waiting for acceptance.
        let sent = Arc::new(Mutex::new(None));
        {
            let mut connect = Box::pin(rpc::connect_server_stream::<Ping, Pong, _>(
                PendingRendezvous {
                    sent: Arc::clone(&sent),
                    complete_send: true,
                },
                4096,
                4096,
            ));
            assert!(poll_once(connect.as_mut()).is_pending());
            drop(connect);
        }

        // Established unary session: drop frees the region.
        {
            let rendezvous = ShmRendezvous::new();
            let server = {
                let rendezvous = rendezvous.clone();
                tokio::spawn(async move {
                    let incoming = rendezvous.recv().await.expect("incoming");
                    let _connection: RpcConnection<Ping, Pong> =
                        rpc::accept(incoming).expect("accept");
                })
            };
            let client = rpc::connect::<Ping, Pong, _>(rendezvous, 0, 0)
                .await
                .expect("connect");
            server.await.expect("server");
            drop(client);
        }

        // Established streaming session: drop frees the region.
        {
            let rendezvous = ShmRendezvous::new();
            let server = {
                let rendezvous = rendezvous.clone();
                tokio::spawn(async move {
                    let incoming = rendezvous.recv().await.expect("incoming");
                    let _connection: ServerStreamConnection<Ping, Pong> =
                        rpc::accept_server_stream(incoming).expect("accept stream");
                })
            };
            let client = rpc::connect_server_stream::<Ping, Pong, _>(rendezvous, 0, 0)
                .await
                .expect("connect stream");
            server.await.expect("server");
            drop(client);
        }

        assert_eq!(
            live_regions(),
            baseline,
            "a create/cancel/drop cycle leaked a session region"
        );
    }
}

//! DNS transaction correlation for the DNS connector.
//!
//! The connector performs real DNS over UDP/53 on behalf of resolving
//! guests. Every upstream query is assigned a transaction id and registered
//! in an in-flight map so that replies — which may arrive out of order — are
//! delivered to exactly the requester that issued them. Unknown transaction
//! ids are dropped: a reply nobody asked for is cross-talk, not an answer.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use selium_proto_dns::{DnsOutcome, DnsResponse, wire::ParsedResponse};
use tokio::sync::mpsc;

/// Shared in-flight map keyed by DNS transaction id.
///
/// The value is the reply channel for the task awaiting that query's
/// response. Allocation and take are both serialised briefly on the mutex;
/// the map itself is a plain `HashMap` — the Q4 pattern in its smallest form.
#[derive(Clone)]
pub struct InFlight {
    inner: Arc<Mutex<State>>,
}

struct State {
    pending: HashMap<u16, mpsc::Sender<DnsResponse>>,
    next_txid: u16,
}

impl InFlight {
    /// Creates an empty in-flight map.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                pending: HashMap::new(),
                next_txid: 1,
            })),
        }
    }

    /// Allocates a fresh transaction id and registers its reply channel.
    ///
    /// Transaction ids are unique across the whole connector (all queries
    /// share one upstream socket), so a reply can never be matched to the
    /// wrong requester.
    pub fn register(&self, reply: mpsc::Sender<DnsResponse>) -> u16 {
        let mut state = self.inner.lock();
        let txid = loop {
            let candidate = state.next_txid;
            state.next_txid = candidate.wrapping_add(1).max(1);
            if !state.pending.contains_key(&candidate) {
                break candidate;
            }
        };
        state.pending.insert(txid, reply);
        txid
    }

    /// Takes the reply channel for a transaction id, if it is still in
    /// flight. Returns `None` for unknown ids — the caller drops the reply.
    pub fn take(&self, txid: u16) -> Option<mpsc::Sender<DnsResponse>> {
        self.inner.lock().pending.remove(&txid)
    }

    /// Number of currently in-flight queries.
    pub fn len(&self) -> usize {
        self.inner.lock().pending.len()
    }

    /// Returns whether the in-flight map is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for InFlight {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a parsed upstream response to the typed [`DnsResponse`] published on
/// the connector's channel.
pub fn response_from_parsed(parsed: &ParsedResponse) -> DnsResponse {
    match parsed.outcome {
        DnsOutcome::Ok => DnsResponse::ok(parsed.addresses.clone()),
        other => DnsResponse::failure(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> mpsc::Sender<DnsResponse> {
        mpsc::channel(1).0
    }

    #[test]
    fn register_allocates_distinct_txids() {
        let inflight = InFlight::new();
        let a = inflight.register(channel());
        let b = inflight.register(channel());
        assert_ne!(a, b);
        assert_eq!(inflight.len(), 2);
    }

    #[test]
    fn take_removes_only_the_requested_txid() {
        let inflight = InFlight::new();
        let a = inflight.register(channel());
        let _b = inflight.register(channel());

        assert!(inflight.take(a).is_some());
        assert_eq!(inflight.len(), 1);
        assert!(inflight.take(a).is_none(), "already consumed");
    }

    #[test]
    fn unknown_txid_is_dropped() {
        // A reply with a transaction id nobody asked for must map to `None`
        // so the connector drops it — no cross-talk between queries.
        let inflight = InFlight::new();
        assert!(inflight.take(0x1234).is_none());
    }

    #[test]
    fn out_of_order_replies_do_not_cross() {
        let inflight = InFlight::new();
        let (first_tx, mut first_rx) = mpsc::channel(1);
        let (second_tx, mut second_rx) = mpsc::channel(1);
        let first_id = inflight.register(first_tx);
        let second_id = inflight.register(second_tx);

        // The second query's reply arrives first.
        let second_channel = inflight.take(second_id).expect("second in flight");
        second_channel
            .try_send(DnsResponse::ok(vec!["127.0.0.1".to_string()]))
            .expect("deliver second reply");
        assert_eq!(
            second_rx.try_recv().expect("second reply").addresses,
            vec!["127.0.0.1".to_string()]
        );

        // The first query's reply is still routed to its own channel.
        let first_channel = inflight.take(first_id).expect("first in flight");
        first_channel
            .try_send(DnsResponse::ok(vec!["93.184.216.34".to_string()]))
            .expect("deliver first reply");
        assert_eq!(
            first_rx.try_recv().expect("first reply").addresses,
            vec!["93.184.216.34".to_string()]
        );
    }

    #[test]
    fn txid_wraps_without_reusing_live_entries() {
        let inflight = InFlight::new();
        let mut ids = Vec::new();
        for _ in 0..100 {
            ids.push(inflight.register(channel()));
        }
        let unique: std::collections::HashSet<u16> = ids.iter().copied().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "no txid may be reused while in flight"
        );
    }

    #[test]
    fn response_from_parsed_ok_carries_addresses() {
        let parsed = ParsedResponse {
            txid: 1,
            outcome: DnsOutcome::Ok,
            addresses: vec!["93.184.216.34".to_string()],
        };
        assert_eq!(
            response_from_parsed(&parsed),
            DnsResponse::ok(vec!["93.184.216.34".to_string()])
        );
    }

    #[test]
    fn response_from_parsed_maps_failure_outcomes() {
        for outcome in [
            DnsOutcome::NxDomain,
            DnsOutcome::Timeout,
            DnsOutcome::Truncated,
            DnsOutcome::ServFail,
            DnsOutcome::Refused,
            DnsOutcome::Upstream,
        ] {
            let parsed = ParsedResponse {
                txid: 1,
                outcome,
                addresses: Vec::new(),
            };
            assert_eq!(response_from_parsed(&parsed).outcome, outcome);
            assert!(response_from_parsed(&parsed).addresses.is_empty());
        }
    }
}

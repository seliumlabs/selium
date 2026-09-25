//! Transport-agnostic live table projected from a pub/sub stream.

use std::{collections::HashMap, hash::Hash, sync::RwLock};

use selium_guest_macros::schema;
use selium_service::FlatMsg;

use crate::{
    MessageTransport,
    error::{Error, Result},
    pubsub::{Publisher, Subscriber},
};

/// A table mutation published over a pub/sub topic.
#[derive(Debug, Clone, PartialEq)]
#[schema(
    path = concat!(env!("CARGO_MANIFEST_DIR"), "/../service/schemas/live_table.fbs"),
    ty = "selium.live_table.LiveTableMessage",
    binding = "selium_service::fbs::selium::live_table::LiveTableMessage",
    wire = LiveTableMessageWire
)]
pub struct LiveTableMessage<K, V> {
    /// Topic-wide mutation id used to acknowledge writes in stream order.
    pub mutation_id: u64,
    /// The entry key.
    pub key: K,
    /// The entry value, or `None` for deletes.
    pub value: Option<V>,
    /// Optional version that must be current for this mutation to apply.
    pub expected_version: Option<u64>,
}

/// A materialised live table record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTableRecord<V> {
    /// The entry value, or `None` for deleted entries (tombstones).
    pub value: Option<V>,
    /// Monotonic version assigned by replay order.
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyOutcome {
    Applied(u64),
    Deleted(u64),
    Conflict { actual: Option<u64> },
}

/// A live table projected from a pub/sub topic stream.
///
/// Writes are published as `LiveTableMessage`s to the underlying topic.
/// Reads are served from a local materialised `HashMap<K, V>`. Remote
/// writes from other processes attached to the same topic are picked up
/// by calling [`sync`](Self::sync).
pub struct LiveTable<K, V, M> {
    publisher: RwLock<Publisher<LiveTableMessage<K, V>, M>>,
    subscriber: RwLock<Subscriber<LiveTableMessage<K, V>, M>>,
    local: RwLock<HashMap<K, LiveTableRecord<V>>>,
}

/// A read-only, materialised view of a live table projected from its pub/sub
/// subscriber.
///
/// Consumers that never write (e.g. the connector's anchor table or the
/// bridge's grant table, both published by the identity guest) attach only a
/// subscriber and project the stream into a local map. Write-side operations
/// are not available; the single writer is the publishing guest.
pub struct LiveTableView<K, V, M> {
    subscriber: RwLock<Subscriber<LiveTableMessage<K, V>, M>>,
    local: RwLock<HashMap<K, LiveTableRecord<V>>>,
}

impl<K, V, M> LiveTable<K, V, M>
where
    K: FlatMsg + Clone + Eq + Hash,
    V: FlatMsg + Clone,
    M: MessageTransport,
{
    /// Creates a live table from an existing publisher/subscriber pair.
    pub fn new(
        publisher: Publisher<LiveTableMessage<K, V>, M>,
        subscriber: Subscriber<LiveTableMessage<K, V>, M>,
    ) -> Result<Self> {
        let table = Self {
            publisher: RwLock::new(publisher),
            subscriber: RwLock::new(subscriber),
            local: RwLock::new(HashMap::new()),
        };
        table.sync()?;
        Ok(table)
    }

    /// Inserts or updates a value, publishing the change to the topic.
    pub fn set(&self, key: K, value: V) -> Result<()> {
        let mut publisher = self
            .publisher
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mutation_id = publisher.allocate_mutation_id();
        let msg = LiveTableMessage {
            mutation_id,
            key,
            value: Some(value),
            expected_version: None,
        };
        publisher.publish(&msg)?;
        drop(publisher);
        self.sync_until_own_mutation(mutation_id)?;
        Ok(())
    }

    /// Inserts or updates a value only when the current version matches `expected_version`.
    pub fn compare_and_set(&self, key: K, expected_version: u64, value: V) -> Result<u64> {
        self.sync()?;
        let actual = self
            .local
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(|record| record.version);
        if actual.unwrap_or(0) != expected_version {
            return Err(Error::CasConflict {
                expected: expected_version,
                actual,
            });
        }

        let mut publisher = self
            .publisher
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mutation_id = publisher.allocate_mutation_id();
        let msg = LiveTableMessage {
            mutation_id,
            key,
            value: Some(value),
            expected_version: Some(expected_version),
        };
        publisher.publish(&msg)?;
        drop(publisher);
        match self.sync_until_own_mutation(mutation_id)? {
            ApplyOutcome::Applied(version) => Ok(version),
            ApplyOutcome::Conflict { actual } => Err(Error::CasConflict {
                expected: expected_version,
                actual,
            }),
            ApplyOutcome::Deleted(version) => Ok(version),
        }
    }

    /// Deletes a value, publishing the deletion to the topic.
    pub fn delete(&self, key: K) -> Result<()> {
        let mut publisher = self
            .publisher
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mutation_id = publisher.allocate_mutation_id();
        let msg = LiveTableMessage {
            mutation_id,
            key,
            value: None,
            expected_version: None,
        };
        publisher.publish(&msg)?;
        drop(publisher);
        self.sync_until_own_mutation(mutation_id)?;
        Ok(())
    }

    /// Returns the value for a key from the local materialised view.
    pub fn get(&self, key: &K) -> Result<Option<V>>
    where
        K: Eq + Hash,
    {
        Ok(self
            .local
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .and_then(|record| record.value.clone()))
    }

    /// Returns the record for a key, including its version.
    pub fn get_record(&self, key: &K) -> Result<Option<LiveTableRecord<V>>>
    where
        K: Eq + Hash,
    {
        Ok(self
            .local
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned())
    }

    /// Returns the current version for a key, including tombstones from deletes.
    pub fn get_version(&self, key: &K) -> Result<Option<u64>>
    where
        K: Eq + Hash,
    {
        Ok(self
            .local
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .map(|entry| entry.version))
    }

    /// Returns up to `limit` records from the local materialised view.
    pub fn scan(&self, limit: usize) -> Result<Vec<(K, LiveTableRecord<V>)>>
    where
        K: Clone + Eq + Hash,
    {
        Ok(scan_entries(
            &self
                .local
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            limit,
        ))
    }

    /// Drains the subscriber to pick up remote writes.
    pub fn sync(&self) -> Result<()> {
        let mut subscriber = self
            .subscriber
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut local = self
            .local
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match subscriber.read_with_tag() {
                Ok((msg, _writer_id)) => {
                    apply_message_to(&mut local, msg);
                }
                Err(Error::BufferEmpty) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    /// Drains the subscriber to pick up remote writes, then awaits the next
    /// remote write.
    ///
    /// Applies every already-buffered mutation, then parks the task on the
    /// transport's `AsyncRead` waker until the next mutation arrives.
    pub async fn sync_async(&self) -> Result<()> {
        self.sync()?;
        std::future::poll_fn(|cx| self.poll_next_message(cx)).await
    }

    /// Applies the next remote mutation, parking on the caller's waker.
    ///
    /// Synchronous poll so the `RwLock` guards never cross an `await` point.
    fn poll_next_message(&self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<()>> {
        let mut subscriber = self
            .subscriber
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match subscriber.read_with_tag() {
            Ok((msg, _writer_id)) => {
                drop(subscriber);
                apply_message_to(
                    &mut self
                        .local
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    msg,
                );
                std::task::Poll::Ready(Ok(()))
            }
            Err(Error::BufferEmpty) => match subscriber.reader_mut().poll_frame(cx) {
                std::task::Poll::Ready(Ok((payload, _tag, _flags))) => {
                    drop(subscriber);
                    let msg: LiveTableMessage<K, V> = match FlatMsg::decode(&payload) {
                        Ok(msg) => msg,
                        Err(e) => {
                            return std::task::Poll::Ready(Err(Error::SerializationFailed(
                                format!("{e}"),
                            )));
                        }
                    };
                    apply_message_to(
                        &mut self
                            .local
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                        msg,
                    );
                    std::task::Poll::Ready(Ok(()))
                }
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            },
            Err(e) => std::task::Poll::Ready(Err(e)),
        }
    }

    /// Inserts or updates a value.
    ///
    /// Publishes the mutation, then parks on the transport's read waker until
    /// this table's own mutation is replayed, applying intervening remote
    /// mutations in order.
    pub async fn set_async(&self, key: K, value: V) -> Result<()> {
        self.publish_and_await(key, Some(value), None).await?;
        Ok(())
    }

    /// Deletes a value, then awaits its replay.
    pub async fn delete_async(&self, key: K) -> Result<()> {
        self.publish_and_await(key, None, None).await?;
        Ok(())
    }

    /// Publishes one mutation and parks until its own replay is applied.
    async fn publish_and_await(
        &self,
        key: K,
        value: Option<V>,
        expected_version: Option<u64>,
    ) -> Result<ApplyOutcome> {
        let mutation_id = {
            let mut publisher = self
                .publisher
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mutation_id = publisher.allocate_mutation_id();
            let msg = LiveTableMessage {
                mutation_id,
                key,
                value,
                expected_version,
            };
            publisher.publish(&msg)?;
            mutation_id
        };
        let own_writer_id = self
            .publisher
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .writer_id();
        std::future::poll_fn(|cx| self.poll_own_mutation(mutation_id, own_writer_id, cx)).await
    }

    /// Applies mutations until this table's own `mutation_id` is replayed.
    ///
    /// Synchronous poll so the `RwLock` guards never cross an `await` point.
    fn poll_own_mutation(
        &self,
        mutation_id: u64,
        own_writer_id: u32,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<ApplyOutcome>> {
        loop {
            let (msg, writer_id) = {
                let mut subscriber = self
                    .subscriber
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match subscriber.reader_mut().poll_frame(cx) {
                    std::task::Poll::Ready(Ok((payload, tag, _flags))) => {
                        drop(subscriber);
                        let msg: LiveTableMessage<K, V> = match FlatMsg::decode(&payload) {
                            Ok(msg) => msg,
                            Err(e) => {
                                return std::task::Poll::Ready(Err(Error::SerializationFailed(
                                    format!("{e}"),
                                )));
                            }
                        };
                        (msg, tag)
                    }
                    std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                }
            };

            let is_own_mutation = writer_id == own_writer_id && msg.mutation_id == mutation_id;
            let outcome = apply_message_to(
                &mut self
                    .local
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                msg,
            );
            if is_own_mutation {
                return std::task::Poll::Ready(Ok(outcome));
            }
        }
    }

    fn sync_until_own_mutation(&self, mutation_id: u64) -> Result<ApplyOutcome> {
        let own_writer_id = self
            .publisher
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .writer_id();
        let mut subscriber = self
            .subscriber
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut local = self
            .local
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match subscriber.read_with_tag() {
                Ok((msg, writer_id)) => {
                    let is_own_mutation =
                        writer_id == own_writer_id && msg.mutation_id == mutation_id;
                    let outcome = apply_message_to(&mut local, msg);
                    if is_own_mutation {
                        return Ok(outcome);
                    }
                }
                Err(Error::BufferEmpty) => return Err(Error::BufferEmpty),
                Err(e) => return Err(e),
            }
        }
    }
}

impl<K, V, M> LiveTableView<K, V, M>
where
    K: FlatMsg + Clone + Eq + Hash,
    V: FlatMsg + Clone,
    M: MessageTransport,
{
    /// Creates a read-only view from a subscriber, draining any already-
    /// buffered mutations into the local view.
    pub fn new(subscriber: Subscriber<LiveTableMessage<K, V>, M>) -> Result<Self> {
        let view = Self {
            subscriber: RwLock::new(subscriber),
            local: RwLock::new(HashMap::new()),
        };
        view.sync()?;
        Ok(view)
    }

    /// Returns the value for a key from the local materialised view.
    pub fn get(&self, key: &K) -> Result<Option<V>>
    where
        K: Eq + Hash,
    {
        Ok(self
            .local
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .and_then(|record| record.value.clone()))
    }

    /// Returns the record for a key, including its version.
    pub fn get_record(&self, key: &K) -> Result<Option<LiveTableRecord<V>>>
    where
        K: Eq + Hash,
    {
        Ok(self
            .local
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned())
    }

    /// Returns up to `limit` records from the local materialised view.
    pub fn scan(&self, limit: usize) -> Result<Vec<(K, LiveTableRecord<V>)>>
    where
        K: Clone + Eq + Hash,
    {
        Ok(scan_entries(
            &self
                .local
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            limit,
        ))
    }

    /// Drains the subscriber to pick up remote writes.
    pub fn sync(&self) -> Result<()> {
        let mut subscriber = self
            .subscriber
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut local = self
            .local
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match subscriber.read_with_tag() {
                Ok((msg, _writer_id)) => {
                    apply_message_to(&mut local, msg);
                }
                Err(Error::BufferEmpty) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    /// Drains the subscriber, then awaits the next remote write.
    pub async fn sync_async(&self) -> Result<()> {
        self.sync()?;
        std::future::poll_fn(|cx| self.poll_next_message(cx)).await
    }

    /// Applies the next remote mutation, parking on the caller's waker.
    fn poll_next_message(&self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<()>> {
        let mut subscriber = self
            .subscriber
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match subscriber.read_with_tag() {
            Ok((msg, _writer_id)) => {
                drop(subscriber);
                apply_message_to(
                    &mut self
                        .local
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    msg,
                );
                std::task::Poll::Ready(Ok(()))
            }
            Err(Error::BufferEmpty) => match subscriber.reader_mut().poll_frame(cx) {
                std::task::Poll::Ready(Ok((payload, _tag, _flags))) => {
                    drop(subscriber);
                    let msg: LiveTableMessage<K, V> = match FlatMsg::decode(&payload) {
                        Ok(msg) => msg,
                        Err(e) => {
                            return std::task::Poll::Ready(Err(Error::SerializationFailed(
                                format!("{e}"),
                            )));
                        }
                    };
                    apply_message_to(
                        &mut self
                            .local
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                        msg,
                    );
                    std::task::Poll::Ready(Ok(()))
                }
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            },
            Err(e) => std::task::Poll::Ready(Err(e)),
        }
    }
}

fn apply_message_to<K, V>(
    local: &mut HashMap<K, LiveTableRecord<V>>,
    msg: LiveTableMessage<K, V>,
) -> ApplyOutcome
where
    K: Eq + Hash,
{
    let actual = local
        .get(&msg.key)
        .map(|record| record.version)
        .unwrap_or(0);
    if let Some(expected) = msg.expected_version
        && actual != expected
    {
        return ApplyOutcome::Conflict {
            actual: local.get(&msg.key).map(|record| record.version),
        };
    }

    let version = actual.saturating_add(1);
    match msg.value {
        Some(value) => {
            local.insert(
                msg.key,
                LiveTableRecord {
                    value: Some(value),
                    version,
                },
            );
            ApplyOutcome::Applied(version)
        }
        None => {
            local.insert(
                msg.key,
                LiveTableRecord {
                    value: None,
                    version,
                },
            );
            ApplyOutcome::Deleted(version)
        }
    }
}

fn scan_entries<K, V>(
    local: &HashMap<K, LiveTableRecord<V>>,
    limit: usize,
) -> Vec<(K, LiveTableRecord<V>)>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    local
        .iter()
        .filter(|(_, record)| record.value.is_some())
        .map(|(key, record)| (key.clone(), record.clone()))
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FramedRead, FramedWrite};

    #[test]
    fn apply_message_versions_records() {
        let mut local = HashMap::new();

        let version = apply_message_to(
            &mut local,
            LiveTableMessage {
                mutation_id: 1,
                key: "alpha".to_string(),
                value: Some(10u64),
                expected_version: None,
            },
        );
        assert_eq!(version, ApplyOutcome::Applied(1));

        let version = apply_message_to(
            &mut local,
            LiveTableMessage {
                mutation_id: 2,
                key: "alpha".to_string(),
                value: Some(20u64),
                expected_version: Some(1),
            },
        );
        assert_eq!(version, ApplyOutcome::Applied(2));
        assert_eq!(local.get("alpha").map(|record| record.version), Some(2));
        assert_eq!(
            local.get("alpha").and_then(|record| record.value),
            Some(20u64)
        );
    }

    #[test]
    fn apply_message_rejects_stale_cas() {
        let mut local = HashMap::from([(
            "alpha".to_string(),
            LiveTableRecord {
                value: Some(10u64),
                version: 2,
            },
        )]);

        let version = apply_message_to(
            &mut local,
            LiveTableMessage {
                mutation_id: 1,
                key: "alpha".to_string(),
                value: Some(20u64),
                expected_version: Some(1),
            },
        );
        assert_eq!(version, ApplyOutcome::Conflict { actual: Some(2) });
        assert_eq!(
            local.get("alpha").and_then(|record| record.value),
            Some(10u64)
        );
    }

    #[test]
    fn apply_message_deletes_records() {
        let mut local = HashMap::from([(
            "alpha".to_string(),
            LiveTableRecord {
                value: Some(10u64),
                version: 1,
            },
        )]);

        apply_message_to(
            &mut local,
            LiveTableMessage {
                mutation_id: 1,
                key: "alpha".to_string(),
                value: None,
                expected_version: None,
            },
        );
        assert_eq!(local.get("alpha").and_then(|record| record.value), None);
        assert_eq!(local.get("alpha").map(|record| record.version), Some(2));
    }

    #[test]
    fn apply_message_recreates_only_with_tombstone_version() {
        let mut local = HashMap::from([(
            "alpha".to_string(),
            LiveTableRecord {
                value: None,
                version: 2,
            },
        )]);

        let stale = apply_message_to(
            &mut local,
            LiveTableMessage {
                mutation_id: 1,
                key: "alpha".to_string(),
                value: Some(10u64),
                expected_version: Some(0),
            },
        );
        assert_eq!(stale, ApplyOutcome::Conflict { actual: Some(2) });

        let recreated = apply_message_to(
            &mut local,
            LiveTableMessage {
                mutation_id: 2,
                key: "alpha".to_string(),
                value: Some(20u64),
                expected_version: Some(2),
            },
        );
        assert_eq!(recreated, ApplyOutcome::Applied(3));
        assert_eq!(
            local.get("alpha").and_then(|record| record.value),
            Some(20u64)
        );
    }

    #[test]
    fn scan_limit_counts_live_records_only() {
        let mut local = HashMap::from([(
            "deleted".to_string(),
            LiveTableRecord {
                value: None,
                version: 2,
            },
        )]);
        apply_message_to(
            &mut local,
            LiveTableMessage {
                mutation_id: 1,
                key: "first".to_string(),
                value: Some(1u64),
                expected_version: None,
            },
        );
        apply_message_to(
            &mut local,
            LiveTableMessage {
                mutation_id: 2,
                key: "second".to_string(),
                value: Some(2u64),
                expected_version: None,
            },
        );

        let live = scan_entries(&local, 2);
        assert_eq!(live.len(), 2);
    }

    /// In-memory duplex transport for the live-table view tests: the
    /// publisher's writes land directly in the subscriber's read stream, no
    /// shared-memory ring required.
    struct TestTransport(tokio::io::DuplexStream);

    impl tokio::io::AsyncRead for TestTransport {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl tokio::io::AsyncWrite for TestTransport {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    impl MessageTransport for TestTransport {
        type Error = std::io::Error;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<bool>> {
            // Let the actual read surface emptiness as BufferEmpty.
            std::task::Poll::Ready(Ok(true))
        }

        fn poll_peer_closed(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<bool>> {
            std::task::Poll::Ready(Ok(false))
        }

        fn generation(&self) -> Result<u64> {
            Ok(0)
        }
    }

    /// The publisher and subscriber halves over the test duplex transport.
    type TestPublisher = Publisher<LiveTableMessage<String, u64>, TestTransport>;
    type TestSubscriber = Subscriber<LiveTableMessage<String, u64>, TestTransport>;

    /// Builds a publisher writing into one duplex half and a subscriber
    /// reading from the other, mirroring a publishing guest and a consumer
    /// attached to its live table.
    fn table_ends() -> (TestPublisher, TestSubscriber) {
        let (writer, reader) = tokio::io::duplex(8 * 1024);
        (
            Publisher::new(FramedWrite::new(TestTransport(writer))),
            Subscriber::new(FramedRead::new(TestTransport(reader)), None),
        )
    }

    fn publish(
        publisher: &mut TestPublisher,
        mutation_id: u64,
        key: &str,
        value: Option<u64>,
        expected_version: Option<u64>,
    ) {
        publisher
            .publish(&LiveTableMessage {
                mutation_id,
                key: key.to_string(),
                value,
                expected_version,
            })
            .expect("publish");
    }

    #[test]
    fn view_materialises_already_published_state() {
        // A consumer attaching after the publisher has already written (e.g.
        // the connector attaching to an identity table that carries
        // replayed anchors) materialises the full stream from position 0.
        let (mut publisher, subscriber) = table_ends();
        publish(&mut publisher, 1, "client-ca-acme", Some(1), None);
        publish(&mut publisher, 2, "client-ca-beta", Some(2), None);
        publish(&mut publisher, 3, "client-ca-acme", None, None);

        let view = LiveTableView::new(subscriber).expect("view");
        assert_eq!(view.get(&"client-ca-acme".to_string()), Ok(None));
        assert_eq!(
            view.get(&"client-ca-beta".to_string()),
            Ok(Some(2u64)),
            "the tombstoned key is absent, the live key visible"
        );
        let scan = view.scan(usize::MAX).expect("scan");
        assert_eq!(scan.len(), 1, "only live records are scanned");
    }

    #[test]
    fn view_sync_picks_up_remote_writes() {
        let (mut publisher, subscriber) = table_ends();
        let view = LiveTableView::new(subscriber).expect("view");

        // Nothing published yet: every key misses.
        assert_eq!(view.get(&"client-ca-acme".to_string()), Ok(None));

        publish(&mut publisher, 1, "client-ca-acme", Some(7), None);
        view.sync().expect("sync");
        assert_eq!(
            view.get(&"client-ca-acme".to_string()),
            Ok(Some(7u64)),
            "a later remote write is visible after sync"
        );
        assert_eq!(
            view.get_record(&"client-ca-acme".to_string())
                .expect("record")
                .map(|record| record.version),
            Some(1)
        );
    }

    #[test]
    fn view_applies_cas_and_rejects_stale_updates() {
        let (mut publisher, subscriber) = table_ends();
        let view = LiveTableView::new(subscriber).expect("view");

        publish(&mut publisher, 1, "alpha", Some(10), None);
        publish(&mut publisher, 2, "alpha", Some(20), Some(1));
        publish(&mut publisher, 3, "alpha", Some(30), Some(1));
        view.sync().expect("sync");

        assert_eq!(
            view.get(&"alpha".to_string()),
            Ok(Some(20u64)),
            "the CAS-matching write applied; the stale one was rejected"
        );
    }

    #[tokio::test]
    async fn view_sync_async_parks_until_the_next_remote_write() {
        let (publisher, subscriber) = table_ends();
        let view = LiveTableView::new(subscriber).expect("view");

        // Park the view on the (empty) stream, then publish from the other
        // side: the parked view wakes and materialises the write.
        let publisher = std::sync::Arc::new(tokio::sync::Mutex::new(publisher));
        let writer = publisher.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let mut publisher = writer.lock().await;
            publish(&mut publisher, 1, "client-ca-acme", Some(1), None);
        });

        view.sync_async().await.expect("sync_async");
        assert_eq!(view.get(&"client-ca-acme".to_string()), Ok(Some(1u64)));
    }
}

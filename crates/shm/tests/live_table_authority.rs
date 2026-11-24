//! Live-table writer authority belongs to the shared ring, not a local label.

use selium_abi::ResourceKind;
use selium_memory::{HeapRegionProvider, set_region_provider};
use selium_shm::{Channel, ChannelBackpressure, ShmTransport};
use selium_wire::{
    FramedRead, FramedWrite, LiveTable, Publisher, Subscriber, TableWriterAuthority, error::Error,
};

type Table = LiveTable<String, u64, ShmTransport>;

fn publisher(
    channel: &Channel,
) -> Publisher<selium_wire::LiveTableMessage<String, u64>, ShmTransport> {
    Publisher::new(FramedWrite::new(
        ShmTransport::new(channel, channel).expect("publisher transport"),
    ))
}

fn subscriber(
    channel: &Channel,
) -> Subscriber<selium_wire::LiveTableMessage<String, u64>, ShmTransport> {
    Subscriber::new(
        FramedRead::new(ShmTransport::new(channel, channel).expect("subscriber transport")),
        None,
    )
}

fn channel() -> Channel {
    Channel::create_with_backpressure(
        64 * 1024,
        ChannelBackpressure::Drop,
        ResourceKind::LiveTable,
    )
    .expect("channel")
}

// scenario: selium-guest/live-table-mutation-has-one-authority/competing-writers-attempt-cas
#[test]
fn attached_channels_compete_on_shared_identity_not_topic_strings() {
    drop(set_region_provider(Box::new(HeapRegionProvider::new())));
    let first_channel = channel();
    let attached = Channel::attach(first_channel.region_id()).expect("cross-guest attachment");

    let first = LiveTable::new(
        TableWriterAuthority::claim("first.local.label").expect("first claim"),
        publisher(&first_channel),
        subscriber(&first_channel),
    )
    .expect("first writer");
    first.set("key".to_string(), 1).expect("initial set");

    let competing: selium_wire::Result<Table> = LiveTable::new(
        TableWriterAuthority::claim("a.completely.different.label").expect("alias claim"),
        publisher(&attached),
        subscriber(&attached),
    );
    assert!(
        matches!(competing, Err(Error::TableAuthorityConflict { .. })),
        "a second attachment must not acquire CAS authority through an alias"
    );

    drop(first);
    let second = LiveTable::new(
        TableWriterAuthority::claim("a.completely.different.label").expect("second claim"),
        publisher(&attached),
        subscriber(&attached),
    )
    .expect("authority is released with the first table");
    second
        .compare_and_set("key".to_string(), 0, 2)
        .expect("the sole attached writer can CAS after the claim transfers");
    assert!(matches!(
        second.compare_and_set("key".to_string(), 0, 3),
        Err(Error::CasConflict { .. })
    ));
    assert_eq!(second.get(&"key".to_string()).expect("get"), Some(2));
    second.delete("key".to_string()).expect("delete");
    assert_eq!(second.get(&"key".to_string()).expect("get"), None);
}

#[test]
fn one_label_on_distinct_channels_does_not_create_a_false_conflict() {
    drop(set_region_provider(Box::new(HeapRegionProvider::new())));
    let first_channel = channel();
    let second_channel = channel();

    let _first = LiveTable::new(
        TableWriterAuthority::claim("same.label").expect("first claim"),
        publisher(&first_channel),
        subscriber(&first_channel),
    )
    .expect("first independent table");
    let _second = LiveTable::new(
        TableWriterAuthority::claim("same.label").expect("second claim"),
        publisher(&second_channel),
        subscriber(&second_channel),
    )
    .expect("the same diagnostic label on another ring is independent");
}

//! Reactive route watching over the discovery route-change table.

use selium_service::ResourceTarget;
use selium_shm::transport::ShmTransport;
use selium_wire::LiveTableView;

use crate::{GuestError, Result, async_runtime::yield_now};

/// The root-namespace route the discovery route-change table is served on.
pub const ROUTE_TABLE_ROUTE: &str = "sel:///discovery-routes";

/// A change observed on a watched route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteEvent {
    /// The route appeared (its first known target).
    Appeared(ResourceTarget),
    /// The route's target was replaced.
    Replaced(ResourceTarget),
    /// The route was revoked or its registration expired.
    Revoked,
}

/// A reactive view of one registry route.
///
/// The watch is seeded from the route's current state (an authoritative
/// one-shot resolve) before deltas from the route-change table are applied, so
/// a consumer that starts before its provider does not miss the appearance, and
/// a consumer that starts after it still observes the current target first.
pub struct RouteWatch {
    uri: String,
    current: Option<ResourceTarget>,
    version: u64,
    pending: Option<RouteEvent>,
    view: LiveTableView<String, ResourceTarget, ShmTransport>,
}

impl RouteWatch {
    pub(crate) fn new(
        uri: String,
        seed: Option<ResourceTarget>,
        version: u64,
        view: LiveTableView<String, ResourceTarget, ShmTransport>,
    ) -> Self {
        Self {
            pending: seed.clone().map(RouteEvent::Appeared),
            current: seed,
            uri,
            version,
            view,
        }
    }

    /// Non-blocking check for a route change.
    pub fn try_next(&mut self) -> Result<Option<RouteEvent>> {
        if let Some(event) = self.pending.take() {
            return Ok(Some(event));
        }
        self.view
            .sync()
            .map_err(|error| GuestError::from(error).context("route watch sync"))?;
        if let Some(record) = self
            .view
            .get_record(&self.uri)
            .map_err(|error| GuestError::from(error).context("route watch read"))?
            && record.version > self.version
        {
            self.version = record.version;
            match record.value {
                Some(target) => {
                    let event = if self.current.is_some() {
                        RouteEvent::Replaced(target.clone())
                    } else {
                        RouteEvent::Appeared(target.clone())
                    };
                    self.current = Some(target);
                    return Ok(Some(event));
                }
                None => {
                    if self.current.take().is_some() {
                        return Ok(Some(RouteEvent::Revoked));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Awaits the next change on the watched route.
    ///
    /// When no change is available the watch *parks on the table* rather than
    /// spinning: a yield loop sets the guest's own wake flag, so any host that
    /// enters the guest repeatedly charges an idle watcher against its
    /// execution budget until the runtime reaps the process. Parking hands the
    /// task back and costs nothing while idle.
    ///
    /// Convergence is wake-driven: a task parked on the table registers its
    /// interest with the host (`WaitRegister`), and the publish that changes
    /// the route — from *any* guest process — advances the region's generation
    /// and delivers a mailbox wake back to this task. The watch therefore
    /// converges on the write itself, not on the host's next poll.
    ///
    /// The yield fallback is for the SDK unit tests, which run outside a host:
    /// there the monotonic-clock hostcall that selects the park path always
    /// fails, and no host exists to wake anything anyway.
    pub async fn next(&mut self) -> Result<RouteEvent> {
        loop {
            if let Some(event) = self.try_next()? {
                return Ok(event);
            }
            match crate::Instant::now() {
                Ok(_) => self
                    .view
                    .sync_async()
                    .await
                    .map_err(|error| GuestError::from(error).context("route watch park"))?,
                Err(_) => yield_now().await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_abi::{ResourceClass, ResourceKind};
    use selium_shm::{Channel, ChannelBackpressure};
    use selium_wire::{
        LiveTable,
        framed::{FramedRead, FramedWrite},
        pubsub::{Publisher, Subscriber},
    };

    fn target(id: u64) -> ResourceTarget {
        ResourceTarget {
            uri: format!("sel://acme/region/{id}"),
            host_id: "host-a".to_string(),
            resource_id: id,
            interface: None,
            tenant: Some("acme".to_string()),
            class: ResourceClass::SharedRegion,
            labels: Vec::new(),
        }
    }

    type Table = LiveTable<String, ResourceTarget, ShmTransport>;

    fn table_and_view() -> (Table, LiveTableView<String, ResourceTarget, ShmTransport>) {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
        let channel = Channel::create_with_backpressure(
            64 * 1024,
            ChannelBackpressure::Drop,
            ResourceKind::LiveTable,
        )
        .expect("channel");
        let write = ShmTransport::new(&channel, &channel).expect("write");
        let read = ShmTransport::new(&channel, &channel).expect("read");
        let publisher = Publisher::new(FramedWrite::new(write));
        let subscriber = Subscriber::new(FramedRead::new(read), None);
        let authority =
            selium_wire::TableWriterAuthority::claim(format!("watch.test.{}", channel.region_id()))
                .expect("authority");
        let table: Table = LiveTable::new(authority, publisher, subscriber).expect("table");

        let read = ShmTransport::new_replay(&channel, &channel).expect("read replay");
        let subscriber = Subscriber::new(FramedRead::new(read), None);
        let view: LiveTableView<String, ResourceTarget, ShmTransport> =
            LiveTableView::new(subscriber).expect("view");
        (table, view)
    }

    #[tokio::test]
    async fn try_next_reports_replacement_and_revocation() {
        let (table, view) = table_and_view();
        let uri = "sel://acme/region/9".to_string();
        let mut watch = RouteWatch::new(uri.clone(), None, 0, view);

        table.set(uri.clone(), target(9)).expect("set");
        assert!(matches!(
            watch.try_next().expect("try_next"),
            Some(RouteEvent::Appeared(_))
        ));

        table.set(uri.clone(), target(10)).expect("replace");
        assert!(matches!(
            watch.try_next().expect("try_next"),
            Some(RouteEvent::Replaced(target)) if target.resource_id == 10
        ));

        table.delete(uri).expect("revoke");
        assert!(matches!(
            watch.try_next().expect("try_next"),
            Some(RouteEvent::Revoked)
        ));
        assert!(watch.try_next().expect("idle").is_none());
    }

    // scenario: guest-context/context-route-watch/guest-watches-a-route-and-converges
    #[tokio::test]
    async fn watch_reports_appearance_when_the_route_appears_later() {
        let (table, view) = table_and_view();
        let uri = "sel://acme/region/7".to_string();
        let mut watch = RouteWatch::new(uri.clone(), None, 0, view);

        table.set(uri.clone(), target(7)).expect("set");
        assert_eq!(
            watch.next().await.expect("next"),
            RouteEvent::Appeared(target(7))
        );
    }

    #[tokio::test]
    async fn watch_seeds_then_reports_replace_and_revoke() {
        let (table, view) = table_and_view();
        let uri = "sel://acme/region/7".to_string();
        table.set(uri.clone(), target(7)).expect("set");
        view.sync().expect("sync");
        let version = view
            .get_record(&uri)
            .expect("get")
            .expect("present")
            .version;

        let mut watch = RouteWatch::new(uri.clone(), Some(target(7)), version, view);
        // The watch seeds from the current state first.
        assert_eq!(
            watch.next().await.expect("seed"),
            RouteEvent::Appeared(target(7))
        );

        table.set(uri.clone(), target(9)).expect("replace");
        assert_eq!(
            watch.next().await.expect("replace"),
            RouteEvent::Replaced(target(9))
        );

        table.delete(uri.clone()).expect("delete");
        assert_eq!(watch.next().await.expect("revoke"), RouteEvent::Revoked);
    }

    // scenario: guest-context/context-route-watch/guest-re-attaches-after-a-provider-restarts
    #[tokio::test]
    async fn watch_re_attaches_after_a_provider_restart() {
        let (table, view) = table_and_view();
        let uri = "sel://acme/region/7".to_string();
        table.set(uri.clone(), target(7)).expect("set");
        view.sync().expect("sync");
        let version = view
            .get_record(&uri)
            .expect("get")
            .expect("present")
            .version;
        let mut watch = RouteWatch::new(uri.clone(), Some(target(7)), version, view);
        assert_eq!(
            watch.next().await.expect("seed"),
            RouteEvent::Appeared(target(7))
        );

        // The provider restarts and re-registers the route with a new resource
        // and an advanced generation: the consumer observes the replacement and
        // can re-attach to the new target.
        table.set(uri.clone(), target(99)).expect("re-register");
        assert!(matches!(
            watch.next().await.expect("replacement"),
            RouteEvent::Replaced(new_target) if new_target.resource_id == 99
        ));
    }
}

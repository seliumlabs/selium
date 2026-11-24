//! SNI-based discovery route resolution with caching.
//!
//! The connector holds no routing table: routes live in discovery as named
//! service routes registered by app guests via
//! [`Context::serve`](selium_guest::Context::serve). A connection is routed
//! once, from the QUIC handshake's server name (SNI); every stream on the
//! connection then goes to that resolved guest. The server name is resolved
//! through the unified [`uri::resolve_wire_name`](selium_abi::uri::resolve_wire_name)
//! — longest-match domain strip against the advisory domain table, or the
//! synthetic tenant label — and the derived internal path is resolved via
//! discovery. The resolver caches the lookup and evicts on attach failure,
//! mirroring the HTTP connector's `RouteResolver`.

use std::{collections::HashMap, sync::Arc};

use selium_abi::uri::{self, DomainTable};
use selium_guest::{Context, GuestError, RouteEvent, RouteWatch};
use selium_service::ResourceTarget;

/// Shared handle to the SNI route resolver, cloned into each connection task.
pub type ResolverHandle = Arc<tokio::sync::Mutex<RouteResolver>>;

/// Resolves a QUIC server name (SNI) to a serving channel via discovery.
pub struct RouteResolver {
    ctx: Option<Context>,
    domains: DomainTable,
    cache: HashMap<String, CachedRoute>,
    reseed_before_resolve: bool,
}

struct CachedRoute {
    target: ResourceTarget,
    /// An unavailable watch forces re-resolution rather than making the cached
    /// target valid forever.
    watch: Option<Box<dyn CachedRouteWatch>>,
}

trait CachedRouteWatch: Send {
    fn try_next(&mut self) -> Result<Option<RouteEvent>, GuestError>;
}

impl CachedRouteWatch for RouteWatch {
    fn try_next(&mut self) -> Result<Option<RouteEvent>, GuestError> {
        RouteWatch::try_next(self)
    }
}

struct IdleWatch;

impl CachedRouteWatch for IdleWatch {
    fn try_next(&mut self) -> Result<Option<RouteEvent>, GuestError> {
        Ok(None)
    }
}

/// Route resolution failures.
#[derive(Debug)]
pub enum ResolveError {
    /// No registration matches the presented server name.
    NotFound,
}

impl RouteResolver {
    /// Creates a resolver backed by the connector's discovery context and the
    /// provisioned advisory domain table (fetched once at startup).
    pub fn new(ctx: Context, domains: DomainTable) -> Self {
        Self {
            ctx: Some(ctx),
            domains,
            cache: HashMap::new(),
            reseed_before_resolve: false,
        }
    }

    /// Evicts a cached route entry, forcing re-resolution on the next
    /// connection for the same name. Called on channel-attach failure.
    /// The next resolution also reconnects discovery so a dead session cannot
    /// park the authoritative lookup indefinitely.
    ///
    /// The supplied name is normalised first: routes are cached under the
    /// canonical (lowercased, trailing-dot-stripped) name, so a raw SNI
    /// must be normalised to hit the cache entry.
    pub fn evict(&mut self, name: &str) {
        self.cache.remove(&normalise_sni(name));
        // A failed session may park forever rather than returning an RPC error.
        // Reconnect before the next lookup instead of awaiting that dead session.
        self.reseed_before_resolve = true;
    }

    /// Returns whether a route is cached for the given name.
    /// Test utility — not for production use.
    pub fn is_cached(&self, name: &str) -> bool {
        self.cache.contains_key(name)
    }

    /// Creates an empty resolver with no context, no table, and no routes.
    /// Test utility.
    pub fn empty() -> Self {
        Self {
            ctx: None,
            domains: DomainTable::new(),
            cache: HashMap::new(),
            reseed_before_resolve: false,
        }
    }

    /// Creates a resolver with a pre-populated cache entry — bypasses
    /// discovery lookup so tests can exercise cache semantics without a
    /// running discovery service.
    pub fn with_cached_route(name: &str, target: ResourceTarget) -> Self {
        let mut cache = HashMap::new();
        cache.insert(
            name.to_string(),
            CachedRoute {
                target,
                watch: Some(Box::new(IdleWatch)),
            },
        );
        Self {
            ctx: None,
            domains: DomainTable::new(),
            cache,
            reseed_before_resolve: false,
        }
    }

    /// Replaces a failed discovery session with the host's current listener.
    async fn reseed_discovery(&mut self) -> Result<(), GuestError> {
        if self.ctx.is_none() {
            return Ok(());
        }
        let ctx = Context::rediscover().await?;
        self.ctx = Some(ctx);
        Ok(())
    }

    /// Resolves through the current session, reconnecting once if it failed.
    async fn lookup(&mut self, uri: &str) -> Result<Option<ResourceTarget>, GuestError> {
        let Some(ctx) = self.ctx.as_mut() else {
            return Ok(None);
        };
        match ctx.lookup(uri).await {
            Ok(target) => Ok(target),
            Err(error) => {
                tracing::warn!("quic-connector: discovery session failed for {uri}: {error}");
                self.reseed_discovery().await?;
                let Some(ctx) = self.ctx.as_mut() else {
                    return Ok(None);
                };
                ctx.lookup(uri).await
            }
        }
    }

    /// Creates a route watch, reconnecting a failed session for the next lookup.
    async fn watch(&mut self, uri: &str) -> Option<Box<dyn CachedRouteWatch>> {
        let result = match self.ctx.as_mut() {
            Some(ctx) => ctx.watch(uri).await,
            None => return None,
        };
        match result {
            Ok(watch) => Some(Box::new(watch)),
            Err(error) => {
                tracing::warn!("quic-connector: route watch failed for {uri}: {error}");
                if let Err(error) = self.reseed_discovery().await {
                    tracing::warn!("quic-connector: discovery rediscovery failed: {error}");
                }
                None
            }
        }
    }

    /// Resolves the serving guest for a server name.
    ///
    /// The name is normalised (lowercased, trailing dot stripped) and
    /// projected through the unified wire-name resolver into an internal
    /// route, which is then resolved via discovery. Resolution happens once
    /// per connection; the connector caches the result.
    pub async fn resolve(&mut self, server_name: &str) -> Result<ResourceTarget, ResolveError> {
        if self.reseed_before_resolve {
            if let Err(error) = self.reseed_discovery().await {
                tracing::warn!("quic-connector: discovery rediscovery failed: {error}");
                return Err(ResolveError::NotFound);
            }
            self.reseed_before_resolve = false;
        }
        let name = normalise_sni(server_name);
        let mut watch_failed = false;
        if let Some(route) = self.cache.get_mut(&name) {
            let re_resolve = match route.watch.as_mut() {
                None => true,
                Some(watch) => match watch.try_next() {
                    Ok(None) => return Ok(route.target.clone()),
                    // A watch's first event is the authoritative seed used to
                    // create it; it is not a route replacement.
                    Ok(Some(RouteEvent::Appeared(target))) if target == route.target => {
                        return Ok(route.target.clone());
                    }
                    Ok(Some(_)) => true,
                    Err(error) => {
                        tracing::warn!(
                            "quic-connector: cached route watch failed for {name}: {error}"
                        );
                        watch_failed = true;
                        true
                    }
                },
            };
            if re_resolve {
                self.cache.remove(&name);
            }
        }
        if watch_failed && let Err(error) = self.reseed_discovery().await {
            tracing::warn!("quic-connector: discovery rediscovery failed: {error}");
            return Err(ResolveError::NotFound);
        }

        let Some((tenant, path)) = uri::resolve_wire_name(&name, &self.domains) else {
            return Err(ResolveError::NotFound);
        };
        // A bare domain (apex) is left for discovery to resolve against the
        // tenant's designated root service.
        let discovery_uri = if path.is_empty() {
            name.clone()
        } else {
            format!("{}{}/{}", uri::SEL_PREFIX, tenant, path.join("/"))
        };

        match self.lookup(&discovery_uri).await {
            Ok(Some(target)) => {
                let watch = self.watch(&target.uri).await;
                self.cache.insert(
                    name,
                    CachedRoute {
                        target: target.clone(),
                        watch,
                    },
                );
                Ok(target)
            }
            Ok(None) => Err(ResolveError::NotFound),
            Err(e) => {
                tracing::warn!("quic-connector: discovery lookup failed for {discovery_uri}: {e}");
                Err(ResolveError::NotFound)
            }
        }
    }
}

/// Normalises a raw SNI server name: lowercased, trailing dot stripped.
fn normalise_sni(server_name: &str) -> String {
    uri::normalise_host(server_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_sni_lowercases_and_strips_trailing_dot() {
        assert_eq!(normalise_sni("Example.COM."), "example.com");
        assert_eq!(normalise_sni("example.com"), "example.com");
    }

    // scenario: addressing/single-canonical-namespace/no-parallel-namespace
    #[test]
    fn resolver_derives_internal_routes_for_synthetic_names() {
        // A synthetic name under the bare tenant label projects directly.
        let table = DomainTable::new();
        assert_eq!(
            uri::resolve_wire_name("bridge.acme", &table),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
    }
    // scenario: addressing/path-label-reversal/deeper-path-reverses-in-full
    // scenario: addressing/unified-wire-name-resolution/sni-resolved-through-the-shared-resolver
    // scenario: quic-connector/sni-based-discovery-route-resolution/registered-domain-sni

    #[test]
    fn resolver_derives_internal_routes_for_registered_domains() {
        let mut table = DomainTable::new();
        table.seed("example.com", "acme");
        assert_eq!(
            uri::resolve_wire_name("bridge.example.com", &table),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
        assert_eq!(
            uri::resolve_wire_name("prod.http.example.com", &table),
            Some((
                "acme".to_string(),
                vec!["http".to_string(), "prod".to_string()]
            ))
        );
    }

    #[test]
    fn resolver_refuses_unknown_names() {
        let mut table = DomainTable::new();
        table.seed("example.com", "acme");
        // No table entry, and the bare label is not a tenant either.
        assert_eq!(
            uri::resolve_wire_name("unknown", &table),
            Some((String::new(), vec!["unknown".to_string()]))
        );
        // A name with no tenant and no route is a NotFound at the resolver.
        let mut resolver = RouteResolver::empty();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = rt.block_on(resolver.resolve("unknown"));
        assert!(matches!(result, Err(ResolveError::NotFound)));
    }

    fn make_target(id: u64) -> ResourceTarget {
        ResourceTarget {
            uri: "sel://acme/bridge".to_string(),
            host_id: String::new(),
            resource_id: id,
            interface: None,
            tenant: Some("acme".to_string()),
            class: selium_abi::ResourceClass::HostQueue,
            labels: Vec::new(),
        }
    }

    struct ScriptedWatch(std::collections::VecDeque<Result<Option<RouteEvent>, GuestError>>);

    impl CachedRouteWatch for ScriptedWatch {
        fn try_next(&mut self) -> Result<Option<RouteEvent>, GuestError> {
            self.0.pop_front().unwrap_or(Ok(None))
        }
    }

    fn resolver_with_watch_event(event: Result<Option<RouteEvent>, GuestError>) -> RouteResolver {
        let mut resolver = RouteResolver::with_cached_route("bridge.acme", make_target(42));
        let route = resolver.cache.get_mut("bridge.acme").expect("seeded route");
        route.watch = Some(Box::new(ScriptedWatch([event].into_iter().collect())));
        resolver
    }

    #[expect(clippy::unwrap_in_result, reason = "test runtime construction")]
    fn resolve_without_discovery(
        resolver: &mut RouteResolver,
    ) -> Result<ResourceTarget, ResolveError> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(resolver.resolve("bridge.acme"))
    }

    #[test]
    fn resolver_evict_removes_cached_entry() {
        let mut resolver = RouteResolver::with_cached_route("bridge.acme", make_target(42));
        assert!(resolver.is_cached("bridge.acme"));
        resolver.evict("bridge.acme");
        assert!(!resolver.is_cached("bridge.acme"));
    }

    // scenario: quic-connector/sni-based-discovery-route-resolution/cache-eviction-normalises-the-raw-sni
    #[test]
    fn resolver_evict_normalises_the_raw_sni() {
        // A route looked up under the raw SNI `Bridge.ACME.` is cached under
        // the normalised name (resolve normalises before caching); eviction
        // must normalise the same way or the stale route would survive.
        let mut resolver = RouteResolver::with_cached_route("bridge.acme", make_target(42));
        assert!(resolver.is_cached("bridge.acme"));
        resolver.evict("Bridge.ACME.");
        assert!(!resolver.is_cached("bridge.acme"));
    }

    #[test]
    fn resolver_evict_of_nonexistent_entry_is_noop() {
        let mut resolver = RouteResolver::with_cached_route("bridge.acme", make_target(42));
        assert!(resolver.is_cached("bridge.acme"));
        resolver.evict("other.acme");
        assert!(resolver.is_cached("bridge.acme"));
    }

    #[test]
    fn resolver_cache_hit_returns_cached_target() {
        let target = make_target(42);
        let mut resolver = RouteResolver::with_cached_route("bridge.acme", target);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = rt.block_on(resolver.resolve("Bridge.ACME."));
        assert_eq!(result.expect("resolve").resource_id, 42);
    }

    #[test]
    fn resolver_cache_miss_without_context_returns_not_found() {
        let mut resolver = RouteResolver::empty();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = rt.block_on(resolver.resolve("bridge.acme"));
        assert!(matches!(result, Err(ResolveError::NotFound)));
    }

    #[test]
    fn initial_watch_appearance_keeps_the_fresh_route_cached() {
        let mut resolver =
            resolver_with_watch_event(Ok(Some(RouteEvent::Appeared(make_target(42)))));

        assert_eq!(
            resolve_without_discovery(&mut resolver)
                .expect("fresh cached target")
                .resource_id,
            42
        );
        assert!(resolver.is_cached("bridge.acme"));
    }

    #[test]
    fn replacement_revocation_and_watch_outage_force_re_resolution() {
        for event in [
            Ok(Some(RouteEvent::Replaced(make_target(43)))),
            Ok(Some(RouteEvent::Revoked)),
            Err(GuestError::Host("watch outage".to_string())),
        ] {
            let mut resolver = resolver_with_watch_event(event);
            assert!(matches!(
                resolve_without_discovery(&mut resolver),
                Err(ResolveError::NotFound)
            ));
            assert!(!resolver.is_cached("bridge.acme"));
        }
    }

    #[test]
    fn failed_watch_creation_does_not_make_the_cache_permanent() {
        let mut resolver = RouteResolver::with_cached_route("bridge.acme", make_target(42));
        resolver
            .cache
            .get_mut("bridge.acme")
            .expect("seeded route")
            .watch = None;

        assert!(matches!(
            resolve_without_discovery(&mut resolver),
            Err(ResolveError::NotFound)
        ));
        assert!(!resolver.is_cached("bridge.acme"));
    }
}

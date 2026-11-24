//! Discovery-based route resolution with caching.
//!
//! The connector holds no routing table: routes live in discovery as named
//! service routes registered by app guests via
//! [`Context::serve`](selium_guest::Context::serve). A request's Host is
//! projected through the unified
//! [`uri::resolve_wire_name`](selium_abi::uri::resolve_wire_name) into a
//! tenant and base path, and the request path is then resolved within that
//! tenant's namespace against the internal routes. The resolver caches
//! lookups per connection-worker and evicts on attach failure, so a stale
//! entry costs one failed request and forces a fresh lookup.

use std::{collections::HashMap, sync::Arc};

use selium_abi::uri::{self, DomainTable};
use selium_guest::{Context, GuestError, RouteEvent, RouteWatch};
use selium_service::ResourceTarget;

/// Test support: re-exports helpers for integration tests in `tests/`.
/// Test utilities — not for production use.
pub mod test_support {
    pub use super::RouteResolver;
}

/// Shared handle to the route resolver, cloned into each connection task.
///
/// The cache is shared across all connections on the listener; lookups
/// are serialised briefly on the mutex, and cache hits avoid discovery
/// round-trips entirely.
pub type ResolverHandle = Arc<tokio::sync::Mutex<RouteResolver>>;

/// Resolves Host + path to a serving channel via discovery lookups.
pub struct RouteResolver {
    ctx: Option<Context>,
    domains: DomainTable,
    cache: HashMap<String, CachedRoute>,
    reseed_before_resolve: bool,
}

struct CachedRoute {
    target: ResourceTarget,
    /// Route-watch for the cached target. An unavailable watch is never treated
    /// as a permanently valid cache entry: the next request re-resolves and
    /// tries to establish a fresh watch.
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
    /// No registration matches the request's Host/path.
    NotFound,
}

impl RouteResolver {
    /// Creates a resolver backed by the connector's discovery context and the
    /// provisioned advisory domain table.
    pub fn new(ctx: Context, domains: DomainTable) -> Self {
        Self {
            ctx: Some(ctx),
            domains,
            cache: HashMap::new(),
            reseed_before_resolve: false,
        }
    }

    /// Evicts a cached route entry, forcing re-resolution on the next
    /// request for the same host+path. Called on session-attach failure.
    /// The next resolution also reconnects discovery so a dead session cannot
    /// park the authoritative lookup indefinitely.
    pub fn evict(&mut self, host: &str, path: &str) {
        let cache_key = format!("{}:{}", host, path);
        self.cache.remove(&cache_key);
        // A failed session may park forever rather than returning an RPC error.
        // Reconnect before the next lookup instead of awaiting that dead session.
        self.reseed_before_resolve = true;
    }

    /// Returns whether a route is cached for the given host+path.
    /// Test utility — not for production use.
    pub fn is_cached(&self, host: &str, path: &str) -> bool {
        let cache_key = format!("{}:{}", host, path);
        self.cache.contains_key(&cache_key)
    }

    /// Creates a RouteResolver with a pre-populated cache entry.
    /// Test utility — bypasses discovery lookup so tests can exercise cache
    /// semantics without a running discovery service.
    pub fn with_cached_route(host: &str, path: &str, target: ResourceTarget) -> Self {
        let mut cache = HashMap::new();
        let cache_key = format!("{}:{}", host, path);
        cache.insert(
            cache_key,
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

    /// Creates an empty resolver with no context and no routes.
    /// Test utility.
    pub fn empty() -> Self {
        Self {
            ctx: None,
            domains: DomainTable::new(),
            cache: HashMap::new(),
            reseed_before_resolve: false,
        }
    }

    /// Creates a resolver with several pre-populated cache entries,
    /// keyed by path for one host. Test utility.
    pub fn with_routes(host: &str, routes: HashMap<String, ResourceTarget>) -> Self {
        let mut cache = HashMap::new();
        for (path, target) in routes {
            let cache_key = format!("{}:{}", host, path);
            cache.insert(
                cache_key,
                CachedRoute {
                    target,
                    watch: Some(Box::new(IdleWatch)),
                },
            );
        }
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
                tracing::warn!("http-connector: discovery session failed for {uri}: {error}");
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
                tracing::warn!("http-connector: route watch failed for {uri}: {error}");
                if let Err(error) = self.reseed_discovery().await {
                    tracing::warn!("http-connector: discovery rediscovery failed: {error}");
                }
                None
            }
        }
    }

    /// Records a resolved route and its watch.
    fn insert_route(
        &mut self,
        cache_key: String,
        target: ResourceTarget,
        watch: Option<Box<dyn CachedRouteWatch>>,
    ) {
        self.cache.insert(cache_key, CachedRoute { target, watch });
    }

    /// Resolves the serving target for a Host + path pair.
    ///
    /// The Host is projected through the unified resolver into a tenant and
    /// base path (`bridge.example.com` → tenant `acme` under `example.com`,
    /// base path `bridge`), then the request path is resolved within that
    /// tenant: exact base+path internal route first, then each parent
    /// subtree (longest prefix first), then the base route itself.
    ///
    /// An apex Host (the bare registered domain, deriving an empty base
    /// path) belongs to the tenant's designated root service: it is
    /// resolved first and handles the request path itself. Without a
    /// designated root service, the request path resolves within the
    /// tenant's namespace directly.
    pub async fn resolve(
        &mut self,
        host: &str,
        path: &str,
    ) -> Result<ResourceTarget, ResolveError> {
        if self.reseed_before_resolve {
            if let Err(error) = self.reseed_discovery().await {
                tracing::warn!("http-connector: discovery rediscovery failed: {error}");
                return Err(ResolveError::NotFound);
            }
            self.reseed_before_resolve = false;
        }
        let host = uri::normalise_host(host);
        let cache_key = format!("{}:{}", host, path);
        let mut watch_failed = false;
        if let Some(route) = self.cache.get_mut(&cache_key) {
            let re_resolve = match route.watch.as_mut() {
                // Watch creation failed during the previous resolution. Never
                // pin the target indefinitely: force discovery resolution and
                // another watch attempt on every request until one succeeds.
                None => true,
                Some(watch) => match watch.try_next() {
                    Ok(None) => return Ok(route.target.clone()),
                    // `Context::watch` yields its authoritative seed first. It
                    // describes the target just cached, not a replacement.
                    Ok(Some(RouteEvent::Appeared(target))) if target == route.target => {
                        return Ok(route.target.clone());
                    }
                    // Replacement, revocation, or a changed seed requires an
                    // authoritative re-resolve through the current session.
                    Ok(Some(_)) => true,
                    Err(error) => {
                        tracing::warn!(
                            "http-connector: cached route watch failed for {cache_key}: {error}"
                        );
                        watch_failed = true;
                        true
                    }
                },
            };
            if re_resolve {
                self.cache.remove(&cache_key);
            }
        }
        if watch_failed && let Err(error) = self.reseed_discovery().await {
            tracing::warn!("http-connector: discovery rediscovery failed: {error}");
            return Err(ResolveError::NotFound);
        }

        let Some((tenant, base_labels)) = uri::resolve_wire_name(&host, &self.domains) else {
            return Err(ResolveError::NotFound);
        };
        let base = base_labels.join("/");

        let segments: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        // Apex: the bare domain maps to the tenant's designated root
        // service, which then owns every request path on the domain. The
        // lookup is answered by discovery (wire name → root service); the
        // root service handles the URL path itself.
        if base.is_empty() {
            match self.lookup(&host).await {
                Ok(Some(target)) => {
                    let watch = self.watch(&target.uri).await;
                    self.insert_route(cache_key.clone(), target.clone(), watch);
                    return Ok(target);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("discovery apex lookup failed for {host}: {e}");
                }
            }
        }

        for uri in candidate_uris(&tenant, &base, &segments) {
            match self.lookup(&uri).await {
                Ok(Some(target)) => {
                    let watch = self.watch(&target.uri).await;
                    self.insert_route(cache_key.clone(), target.clone(), watch);
                    return Ok(target);
                }
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!("discovery lookup failed for {uri}: {e}");
                    continue;
                }
            }
        }

        Err(ResolveError::NotFound)
    }
}

/// Builds the internal route candidates for a tenant, a wire-name base
/// path, and the request path segments: exact base+segments first, then
/// each parent subtree (longest prefix first), then the base route itself.
/// A bare tenant root (empty base and no request path) is not addressable.
fn candidate_uris(tenant: &str, base: &str, segments: &[&str]) -> Vec<String> {
    let mut candidates = Vec::with_capacity(segments.len() + 1);
    for len in (0..=segments.len()).rev() {
        let suffix = segments.get(..len).unwrap_or_default().join("/");
        let internal_path = match (base.is_empty(), suffix.is_empty()) {
            (true, true) => continue, // bare tenant root is not addressable
            (true, false) => suffix,
            (false, true) => base.to_string(),
            (false, false) => format!("{base}/{suffix}"),
        };
        candidates.push(format!("{}{}/{}", uri::SEL_PREFIX, tenant, internal_path));
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    // scenario: addressing/unified-wire-name-resolution/host-resolved-through-the-shared-resolver
    #[test]
    fn host_derives_tenant_and_path() {
        let mut table = DomainTable::new();
        table.seed("example.com", "acme");
        assert_eq!(
            uri::resolve_wire_name("bridge.example.com", &table),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
        assert_eq!(
            uri::resolve_wire_name("bridge.acme", &table),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
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
        let target = make_target(42);
        let mut resolver = RouteResolver::with_cached_route("example.com", "/test", target);
        let route = resolver
            .cache
            .get_mut("example.com:/test")
            .expect("seeded route");
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
            .block_on(resolver.resolve("example.com", "/test"))
    }

    #[test]
    fn candidates_walk_longest_internal_path_first() {
        // Host `bridge.example.com` (base `bridge`) + `/api/v2` resolves
        // base+path first, then parents, then the base route.
        assert_eq!(
            candidate_uris("acme", "bridge", &["api", "v2"]),
            vec![
                "sel://acme/bridge/api/v2",
                "sel://acme/bridge/api",
                "sel://acme/bridge",
            ]
        );
        // A deeper base composes the same way.
        assert_eq!(
            candidate_uris("acme", "http/prod", &[]),
            vec!["sel://acme/http/prod"]
        );
    }

    #[test]
    fn apex_candidates_fall_back_to_tenant_namespace_paths() {
        // An apex host (empty base) with no designated root service falls
        // back to the tenant's own namespace paths; the bare tenant root is
        // not addressable.
        assert_eq!(
            candidate_uris("acme", "", &["healthz"]),
            vec!["sel://acme/healthz"]
        );
        assert!(candidate_uris("acme", "", &[]).is_empty());
    }

    #[test]
    fn apex_wire_name_derives_an_empty_base() {
        // The apex branch is reached only via a registered domain: the bare
        // domain strips to (tenant, no labels), which is what triggers the
        // root-service lookup in `resolve`.
        let mut table = DomainTable::new();
        table.seed("example.com", "acme");
        assert_eq!(
            uri::resolve_wire_name("example.com", &table),
            Some(("acme".to_string(), Vec::new()))
        );
        // The root service itself is designated in discovery; resolving the
        // bare wire name maps the apex onto it (store-level coverage:
        // `root_service_designation_resolves_the_apex`).
    }

    #[test]
    fn route_resolver_evict_removes_cached_entry() {
        let target = make_target(42);
        let mut resolver = RouteResolver::with_cached_route("example.com", "/test", target);

        assert!(resolver.is_cached("example.com", "/test"));
        resolver.evict("example.com", "/test");
        assert!(!resolver.is_cached("example.com", "/test"));
    }

    #[test]
    fn route_resolver_evict_of_nonexistent_entry_is_noop() {
        let target = make_target(42);
        let mut resolver = RouteResolver::with_cached_route("example.com", "/api", target);

        assert!(resolver.is_cached("example.com", "/api"));
        assert!(!resolver.is_cached("example.com", "/other"));

        resolver.evict("example.com", "/other");
        assert!(resolver.is_cached("example.com", "/api"));

        resolver.evict("example.com", "/api");
        assert!(!resolver.is_cached("example.com", "/api"));
    }

    #[test]
    fn route_resolver_cache_hit_returns_cached_target() {
        let target = make_target(42);
        let mut resolver = RouteResolver::with_cached_route("example.com", "/test", target);

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = rt.block_on(resolver.resolve("example.com", "/test"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().resource_id, 42);
    }

    #[test]
    fn route_resolver_cache_miss_without_context_returns_not_found() {
        let mut resolver =
            RouteResolver::with_cached_route("example.com", "/cached-only", make_target(7));

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = rt.block_on(resolver.resolve("example.com", "/not-cached"));
        assert!(matches!(result, Err(ResolveError::NotFound)));
    }

    #[test]
    fn route_resolver_stale_entry_not_reused_after_eviction() {
        let target = make_target(42);
        let mut resolver = RouteResolver::with_cached_route("example.com", "/test", target);

        resolver.evict("example.com", "/test");
        assert!(!resolver.is_cached("example.com", "/test"));

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = rt.block_on(resolver.resolve("example.com", "/test"));
        assert!(matches!(result, Err(ResolveError::NotFound)));
    }

    #[test]
    fn initial_watch_appearance_does_not_evict_the_fresh_route() {
        let mut resolver =
            resolver_with_watch_event(Ok(Some(RouteEvent::Appeared(make_target(42)))));

        let resolved = resolve_without_discovery(&mut resolver).expect("fresh cached target");

        assert_eq!(resolved.resource_id, 42);
        assert!(resolver.is_cached("example.com", "/test"));
    }

    #[test]
    fn watch_replacement_forces_an_authoritative_re_resolve() {
        let mut resolver =
            resolver_with_watch_event(Ok(Some(RouteEvent::Replaced(make_target(43)))));

        assert!(matches!(
            resolve_without_discovery(&mut resolver),
            Err(ResolveError::NotFound)
        ));
        assert!(!resolver.is_cached("example.com", "/test"));
    }

    #[test]
    fn watch_revocation_forces_an_authoritative_re_resolve() {
        let mut resolver = resolver_with_watch_event(Ok(Some(RouteEvent::Revoked)));

        assert!(matches!(
            resolve_without_discovery(&mut resolver),
            Err(ResolveError::NotFound)
        ));
        assert!(!resolver.is_cached("example.com", "/test"));
    }

    #[test]
    fn watch_failure_and_creation_outage_never_pin_a_stale_route() {
        let mut without_watch =
            RouteResolver::with_cached_route("example.com", "/test", make_target(42));
        without_watch
            .cache
            .get_mut("example.com:/test")
            .expect("seeded route")
            .watch = None;
        for mut resolver in [
            resolver_with_watch_event(Err(GuestError::Host("watch outage".to_string()))),
            without_watch,
        ] {
            assert!(matches!(
                resolve_without_discovery(&mut resolver),
                Err(ResolveError::NotFound)
            ));
            assert!(!resolver.is_cached("example.com", "/test"));
        }
    }
}

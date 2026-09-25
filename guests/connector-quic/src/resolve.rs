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
use selium_guest::Context;
use selium_service::ResourceTarget;

/// Shared handle to the SNI route resolver, cloned into each connection task.
pub type ResolverHandle = Arc<tokio::sync::Mutex<RouteResolver>>;

/// Resolves a QUIC server name (SNI) to a serving channel via discovery.
pub struct RouteResolver {
    ctx: Option<Context>,
    domains: DomainTable,
    cache: HashMap<String, CachedRoute>,
}

#[derive(Clone)]
struct CachedRoute {
    target: ResourceTarget,
    _created_at_ms: u64,
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
        }
    }

    /// Evicts a cached route entry, forcing re-resolution on the next
    /// connection for the same name. Called on channel-attach failure.
    ///
    /// The supplied name is normalised first: routes are cached under the
    /// canonical (lowercased, trailing-dot-stripped) name, so a raw SNI
    /// must be normalised to hit the cache entry.
    pub fn evict(&mut self, name: &str) {
        self.cache.remove(&normalize_sni(name));
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
                _created_at_ms: 0,
            },
        );
        Self {
            ctx: None,
            domains: DomainTable::new(),
            cache,
        }
    }

    /// Resolves the serving guest for a server name.
    ///
    /// The name is normalised (lowercased, trailing dot stripped) and
    /// projected through the unified wire-name resolver into an internal
    /// route, which is then resolved via discovery. Resolution happens once
    /// per connection; the connector caches the result.
    pub async fn resolve(&mut self, server_name: &str) -> Result<ResourceTarget, ResolveError> {
        let name = normalize_sni(server_name);
        if let Some(route) = self.cache.get(&name) {
            return Ok(route.target.clone());
        }

        let Some(ref mut ctx) = self.ctx else {
            return Err(ResolveError::NotFound);
        };

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

        match ctx.lookup(&discovery_uri).await {
            Ok(Some(target)) => {
                self.cache.insert(
                    name,
                    CachedRoute {
                        target: target.clone(),
                        _created_at_ms: 0,
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
fn normalize_sni(server_name: &str) -> String {
    uri::normalize_host(server_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_sni_lowercases_and_strips_trailing_dot() {
        assert_eq!(normalize_sni("Example.COM."), "example.com");
        assert_eq!(normalize_sni("example.com"), "example.com");
    }

    #[test]
    fn resolver_derives_internal_routes_for_synthetic_names() {
        // A synthetic name under the bare tenant label projects directly.
        let table = DomainTable::new();
        assert_eq!(
            uri::resolve_wire_name("bridge.acme", &table),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
    }

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

    #[test]
    fn resolver_evict_removes_cached_entry() {
        let mut resolver = RouteResolver::with_cached_route("bridge.acme", make_target(42));
        assert!(resolver.is_cached("bridge.acme"));
        resolver.evict("bridge.acme");
        assert!(!resolver.is_cached("bridge.acme"));
    }

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
}

//! Discovery probe test fixture guest.
//!
//! Minimal application guest used by the discovery and failure-injection
//! integration tests. Receives its bootstrap discovery `Context` (built by the
//! entrypoint macro, exercising the discovery rendezvous), allocates a
//! shared-memory region, logs its progress, and marks ready.
//!
//! Roles are selected by the trailing integer entrypoint argument:
//!
//! - [`ROLE_PROVIDER`] registers a route in the discovery registry through the
//!   SDK (which also renews its lease) and stays resident.
//! - [`ROLE_CONSUMER`] watches a route through [`selium_guest::Context::watch`]
//!   and logs every event it observes.
//! - [`ROLE_RESOLVER`] resolves a URI once and logs `found` or `notfound`.
//! - [`ROLE_REGISTER_NO_RENEW`] registers through the raw discovery client and
//!   deliberately never renews, so the lease sweep must expire it.
//! - [`ROLE_TRAP_AFTER_REGISTER`] registers and then crashes, so the runtime's
//!   failure path (not the graceful stop path) has to revoke the route.
//! - [`ROLE_FORGE`] tries to register a reserved root route without holding
//!   its claim; discovery must refuse it.
//! - [`ROLE_CLAIM_ROUTE`] registers a reserved handler route under a
//!   host-provisioned claim and stays resident, so a second claimant can be
//!   refused with a conflict.
//! - [`ROLE_CONNECTOR_PROVIDER`] publishes a process target for the connector
//!   resolver recovery test.
//! - [`ROLE_CONNECTOR_RESOLVER`] runs the real HTTP and QUIC route resolvers
//!   until both converge on replacement providers.

use anyhow::{Context as _, bail};
use selium_abi::uri::DomainTable;
use selium_connector_resolve::{
    http::RouteResolver as HttpRouteResolver, quic::RouteResolver as QuicRouteResolver,
};
use selium_guest::{
    Context, ResourceClass, ResourceTarget, RouteEvent, Serve, entrypoint, info, process_id,
};
use selium_service::{DiscoveryRequest, DiscoveryResponse};
use selium_shm::{Channel, ChannelBackpressure};

/// Channel capacity for the probe region.
const PROBE_CHANNEL_CAPACITY: u64 = 4096;
/// How long a resident role stays parked between checks. Long enough never to
/// elapse during a test; these entrypoints are services, not one-shot jobs.
const PROVIDER_PARK: std::time::Duration = std::time::Duration::from_secs(3_600);
/// Readiness bound for the converging role: long enough for a provider the
/// test starts after boot, short enough to fail a hung wait.
const READY_BOUND: std::time::Duration = std::time::Duration::from_secs(30);
/// Readiness bound for the timeout role: a route nobody publishes.
const READY_TIMEOUT_BOUND: std::time::Duration = std::time::Duration::from_millis(500);
/// Register a reserved handler route under a host-provisioned claim.
const ROLE_CLAIM_ROUTE: u64 = 7;
/// Watch a route and log every change.
const ROLE_CONSUMER: u64 = 2;
/// Try to register a reserved root route without its claim.
const ROLE_FORGE: u64 = 6;
/// Register a route (SDK path, with lease renewal) and stay resident.
const ROLE_PROVIDER: u64 = 1;
/// Converge on a route through the bounded readiness path and log the outcome.
const ROLE_READY: u64 = 9;
/// Resolve a URI once and log how long the immediate miss took.
const ROLE_READY_MISS: u64 = 11;
/// Resolve on demand through the bounded readiness path (short bound).
const ROLE_READY_TIMEOUT: u64 = 10;
/// Allocate a region, log, and return.
const ROLE_REGION: u64 = 0;
/// Register through the raw client and never renew the lease.
const ROLE_REGISTER_NO_RENEW: u64 = 4;
/// Resolve a URI once and log the answer.
const ROLE_RESOLVER: u64 = 3;
/// Resolve a URI until it is gone, logging each answer.
const ROLE_RESOLVER_UNTIL_GONE: u64 = 8;
/// Serve an owned resource with its attachability declaration refused by the
/// host (armed through the runtime's test fault seam), then report whether
/// either route was published.
const ROLE_SERVE_DECLARATION_FAILS: u64 = 12;
/// List the discovery service's durable domain mappings and stay resident.
pub const ROLE_DOMAINS: u64 = 13;
/// Publish one process-backed route for the connector resolver test.
const ROLE_CONNECTOR_PROVIDER: u64 = 14;
/// Resolve HTTP and QUIC routes until their providers are replaced.
const ROLE_CONNECTOR_RESOLVER: u64 = 15;
/// Register, then crash (a trap, not a graceful exit).
const ROLE_TRAP_AFTER_REGISTER: u64 = 5;

/// Registers a reserved handler route as a root service under the caller's
/// host-provisioned handler claim, then stays resident.
async fn claim_route(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    let target = ResourceTarget {
        uri: uri.to_string(),
        host_id: "discovery-probe".to_string(),
        resource_id: process_id(),
        interface: None,
        tenant: None,
        class: ResourceClass::Process,
        labels: Vec::new(),
    };
    match request_register(ctx, uri, target, true).await? {
        DiscoveryResponse::Registered => info!("claim: registered {uri}"),
        other => info!("claim: refused {uri} {other:?}"),
    }
    park().await
}

/// Watches `uri` and logs every event, forever.
async fn consumer(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    let mut watch = ctx
        .watch(uri)
        .await
        .with_context(|| format!("consumer: watch {uri}"))?;
    info!("consumer: watching {uri}");

    loop {
        match watch.next().await.context("consumer: watch")? {
            RouteEvent::Appeared(target) => {
                info!("consumer: appeared {uri} {}", target.resource_id);
            }
            RouteEvent::Replaced(target) => {
                info!("consumer: replaced {uri} {}", target.resource_id);
            }
            RouteEvent::Revoked => {
                info!("consumer: revoked {uri}");
            }
        }
    }
}

#[entrypoint]
async fn discovery_probe(mut ctx: Context, route: (u64, u64), mode: u64) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    match mode {
        ROLE_REGION => {
            info!(guest = "discovery-probe", "booting");

            // Allocate a shared-memory channel. The host records its authority
            // synchronously; the owner (not the runtime) publishes any
            // registration for it.
            let channel = Channel::create(PROBE_CHANNEL_CAPACITY, ChannelBackpressure::Park)?;
            info!(region_id = channel.region_id(), "probe: region allocated");

            info!("guest ready");
            // Stay resident: the test drains this guest's log channel and then
            // stops the process to assert revocation. A guest that returns
            // immediately may be reaped (taking its log channel) before the
            // lines are read.
            park().await
        }
        ROLE_PROVIDER => provider(&mut ctx, route_uri(route)).await,
        ROLE_CONSUMER => consumer(&mut ctx, route_uri(route)).await,
        ROLE_RESOLVER => resolver(&mut ctx, route_uri(route)).await,
        ROLE_REGISTER_NO_RENEW => register_no_renew(&mut ctx, route_uri(route)).await,
        ROLE_TRAP_AFTER_REGISTER => trap_after_register(&mut ctx, route_uri(route)).await,
        ROLE_FORGE => forge(&mut ctx, route_uri(route)).await,
        ROLE_CLAIM_ROUTE => claim_route(&mut ctx, route_uri(route)).await,
        ROLE_RESOLVER_UNTIL_GONE => resolver_until_gone(&mut ctx, route_uri(route)).await,
        ROLE_READY => ready(&mut ctx, route_uri(route), READY_BOUND).await,
        ROLE_READY_TIMEOUT => ready(&mut ctx, route_uri(route), READY_TIMEOUT_BOUND).await,
        ROLE_READY_MISS => ready_miss(&mut ctx, route_uri(route)).await,
        ROLE_SERVE_DECLARATION_FAILS => serve_declaration_fails(&mut ctx).await,
        ROLE_DOMAINS => domains(&mut ctx).await,
        ROLE_CONNECTOR_PROVIDER => connector_provider(&mut ctx, route_uri(route)).await,
        ROLE_CONNECTOR_RESOLVER => connector_resolvers(ctx).await,
        other => bail!("discovery-probe: unknown mode {other}"),
    }
}

/// Tries to register a reserved root route without holding its claim.
async fn forge(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    let channel = Channel::create(PROBE_CHANNEL_CAPACITY, ChannelBackpressure::Park)
        .context("forge: create channel")?;
    let target = region_target(uri, channel.region_id());
    match request_register(ctx, uri, target, true).await? {
        DiscoveryResponse::Registered => info!("forge: registered {uri} (unexpected)"),
        other => info!("forge: refused {uri} {other:?}"),
    }
    Ok(())
}

/// Stays resident without doing work; the entrypoint is a service.
async fn park() -> anyhow::Result<()> {
    loop {
        let now = selium_guest::Instant::now().context("park: clock")?;
        let deadline = now
            .checked_add(PROVIDER_PARK)
            .unwrap_or(selium_guest::Instant::MAX);
        selium_guest::Timer::new(deadline).await?;
    }
}

/// Registers `uri` in the registry (with the SDK's lease renewal) and stays
/// resident as its provider.
async fn provider(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    let channel = Channel::create(PROBE_CHANNEL_CAPACITY, ChannelBackpressure::Park)
        .context("provider: create channel")?;
    let resource_id = channel.region_id();
    let target = region_target(uri, resource_id);

    // Registering immediately after minting the region must be admitted on the
    // first attempt: the registry asks the host whether this process owns the
    // target, rather than waiting for its own Tier-1 view to catch up. This
    // fixture deliberately does *not* retry, so a regression in that admission
    // path shows up here as a failed provider rather than as a slow one.
    ctx.register(uri, target)
        .await
        .map_err(|error| anyhow::anyhow!("provider: register {uri}: {error}"))?;
    info!("provider: registered {uri} {resource_id}");
    park().await
}

/// Publishes this process as one root route used by the connector resolvers.
async fn connector_provider(ctx: &mut Context, route: &str) -> anyhow::Result<()> {
    let uri = format!("sel:///{route}");
    let process_id = process_id();
    let target = ResourceTarget {
        uri: uri.clone(),
        host_id: "discovery-probe".to_string(),
        resource_id: process_id,
        interface: None,
        tenant: None,
        class: ResourceClass::Process,
        labels: Vec::new(),
    };
    ctx.register(&uri, target)
        .await
        .map_err(|error| anyhow::anyhow!("connector-provider: register {uri}: {error}"))?;
    info!("connector-provider: registered {uri} {process_id}");
    park().await
}

/// Runs the production HTTP and QUIC resolvers through a real discovery swap.
async fn connector_resolvers(ctx: Context) -> anyhow::Result<()> {
    let quic_ctx = Context::rediscover()
        .await
        .context("connector-resolver: connect second discovery session")?;
    let mut domains = DomainTable::new();
    // Map both names into the root namespace without using an apex root
    // service: http.test -> sel:///http and quic.test -> sel:///quic.
    domains.seed("test", "");
    let mut http = HttpRouteResolver::new(ctx, domains.clone());
    let mut quic = QuicRouteResolver::new(quic_ctx, domains);

    let http_initial = http
        .resolve("http.test", "/")
        .await
        .map_err(|error| anyhow::anyhow!("connector-resolver: initial HTTP lookup: {error:?}"))?
        .resource_id;
    let quic_initial = quic
        .resolve("quic.test")
        .await
        .map_err(|error| anyhow::anyhow!("connector-resolver: initial QUIC lookup: {error:?}"))?
        .resource_id;
    info!("connector-resolver: http initial {http_initial}");
    info!("connector-resolver: quic initial {quic_initial}");

    // Give the host time to replace discovery and both providers, then follow
    // the connector's attach-failure path. This forces an authoritative lookup
    // through the stale session instead of allowing an unchanged cached target
    // to short-circuit the recovery assertion.
    let now = selium_guest::Instant::now().context("connector-resolver: clock")?;
    let deadline = now
        .checked_add(std::time::Duration::from_secs(5))
        .unwrap_or(selium_guest::Instant::MAX);
    selium_guest::Timer::new(deadline).await?;
    http.evict("http.test", "/");
    quic.evict("quic.test");
    info!("connector-resolver: re-resolving after eviction");

    let mut http_recovered = false;
    let mut quic_recovered = false;
    loop {
        if !http_recovered {
            match http.resolve("http.test", "/").await {
                Ok(target) if target.resource_id != http_initial => {
                    http_recovered = true;
                    info!("connector-resolver: http recovered {}", target.resource_id);
                }
                Ok(_) => {}
                Err(error) => info!("connector-resolver: http waiting {error:?}"),
            }
        }
        if !quic_recovered {
            match quic.resolve("quic.test").await {
                Ok(target) if target.resource_id != quic_initial => {
                    quic_recovered = true;
                    info!("connector-resolver: quic recovered {}", target.resource_id);
                }
                Ok(_) => {}
                Err(error) => info!("connector-resolver: quic waiting {error:?}"),
            }
        }
        if http_recovered && quic_recovered {
            return park().await;
        }
        let now = selium_guest::Instant::now().context("connector-resolver: clock")?;
        let deadline = now
            .checked_add(std::time::Duration::from_millis(250))
            .unwrap_or(selium_guest::Instant::MAX);
        selium_guest::Timer::new(deadline).await?;
    }
}

/// Converges on `uri` through the bounded watch-driven readiness path.
///
/// Logs the resolved resource on success, or the typed timeout on expiry. A
/// resident role must never spin: the wait parks on the route table.
async fn ready(ctx: &mut Context, uri: &str, bound: std::time::Duration) -> anyhow::Result<()> {
    match ctx.resolve_ready(uri, bound).await {
        Ok(target) => info!("ready: found {uri} {}", target.resource_id),
        Err(error) => info!("ready: timeout {uri} ({error})"),
    }
    // Stay resident so the probe's log channel survives until the test has
    // drained the outcome (a guest that exits immediately may take its channel
    // with it).
    park().await
}

/// Resolves `uri` exactly once and logs how long the immediate miss took.
///
/// The request path must not wait for the readiness bound: a genuine miss is
/// `NotFound` immediately.
async fn ready_miss(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    let started = selium_guest::Instant::now()?;
    let found = ctx.lookup(uri).await?;
    let elapsed = selium_guest::Instant::now()?
        .checked_duration_since(started)
        .unwrap_or_default();
    let outcome = if found.is_some() { "found" } else { "notfound" };
    info!("miss: {outcome} {uri} {}ms", elapsed.as_millis());
    Ok(())
}

/// Builds a region-backed target for `uri`.
fn region_target(uri: &str, resource_id: u64) -> ResourceTarget {
    ResourceTarget {
        uri: uri.to_string(),
        host_id: "discovery-probe".to_string(),
        resource_id,
        interface: None,
        tenant: None,
        class: ResourceClass::SharedRegion,
        labels: Vec::new(),
    }
}

/// Registers `uri` through the raw client and never renews its lease.
///
/// The lease is set at registration time; with no `Renew` request, the
/// discovery sweep must expire the registration once its TTL has lapsed.
async fn register_no_renew(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    let channel = Channel::create(PROBE_CHANNEL_CAPACITY, ChannelBackpressure::Park)
        .context("lease: create channel")?;
    let resource_id = channel.region_id();
    match request_register(ctx, uri, region_target(uri, resource_id), false).await? {
        DiscoveryResponse::Registered => {
            info!("lease: registered {uri} {resource_id} without renewal");
        }
        other => bail!("lease: registration was not admitted: {other:?}"),
    }
    park().await
}

/// Sends a raw registration through the discovery client, bypassing the SDK's
/// lease-renewal bookkeeping.
async fn request_register(
    ctx: &mut Context,
    uri: &str,
    target: ResourceTarget,
    root_service: bool,
) -> anyhow::Result<DiscoveryResponse> {
    ctx.discovery()
        .request(DiscoveryRequest::Register {
            uri: uri.to_string(),
            target,
            owner: None,
            root_service,
        })
        .await
        .context("register: discovery request")
}

/// Lists every domain mapping returned by the current discovery listener.
async fn domains(ctx: &mut Context) -> anyhow::Result<()> {
    loop {
        let domains = ctx.load_domains().await.context("domains: list")?;
        for (domain, tenant) in domains.entries() {
            info!("domains: {domain}={tenant}");
        }
        let now = selium_guest::Instant::now().context("domains: clock")?;
        let deadline = now
            .checked_add(std::time::Duration::from_millis(250))
            .unwrap_or(selium_guest::Instant::MAX);
        selium_guest::Timer::new(deadline).await?;
    }
}

/// Resolves `uri` once and logs the observable outcome.
async fn resolver(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    let response = ctx
        .discovery()
        .request(DiscoveryRequest::Resolve(uri.to_string()))
        .await
        .context("resolver: resolve")?;
    match response {
        DiscoveryResponse::Found(target) => {
            info!("resolver: found {uri} {}", target.resource_id);
        }
        DiscoveryResponse::NotFound => info!("resolver: notfound {uri}"),
        other => info!("resolver: other {uri} {other:?}"),
    }
    Ok(())
}

/// Resolves `uri` periodically until it reports `NotFound`.
///
/// A single resolve races the discovery sweep; a lapsed-lease test must keep
/// asking until the sweep has ticked past the deadline.
async fn resolver_until_gone(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    loop {
        let response = ctx
            .discovery()
            .request(DiscoveryRequest::Resolve(uri.to_string()))
            .await
            .context("resolver: resolve")?;
        match response {
            DiscoveryResponse::NotFound => {
                info!("resolver: notfound {uri}");
                return Ok(());
            }
            DiscoveryResponse::Found(target) => {
                info!("resolver: found {uri} {}", target.resource_id);
            }
            other => info!("resolver: other {uri} {other:?}"),
        }
        let now = selium_guest::Instant::now().context("resolver: clock")?;
        let deadline = now
            .checked_add(std::time::Duration::from_millis(250))
            .unwrap_or(selium_guest::Instant::MAX);
        selium_guest::Timer::new(deadline).await?;
    }
}

/// Decodes the route URI argument the runtime wrote into guest memory.
fn route_uri(route: (u64, u64)) -> &'static str {
    // SAFETY: `route` is this entrypoint's pointer argument; the runtime
    // populated it with the descriptor's argument bytes before the call.
    unsafe { selium_guest::args::str(route.0, route.1) }.unwrap_or_default()
}

/// Calls `Context::serve` for a resource this guest owns while the host refuses
/// the attachability declaration, then reports whether either route exists.
///
/// The declaration is supposed to happen before any publication, so a refused
/// declaration must leave both the canonical typed route and the named service
/// route unpublished.
async fn serve_declaration_fails(ctx: &mut Context) -> anyhow::Result<()> {
    let channel = Channel::create(PROBE_CHANNEL_CAPACITY, ChannelBackpressure::Park)
        .context("serve-decl: create channel")?;
    let resource_id = channel.region_id();
    let (_, tenant) = selium_guest::self_info().context("serve-decl: self info")?;
    let tenant = tenant.unwrap_or_default();
    let path = ["decl", "fail"];

    let target = ResourceTarget {
        uri: String::new(), // pinned by `serve` to the derived internal path
        host_id: "discovery-probe".to_string(),
        resource_id,
        interface: None,
        tenant: None,
        class: ResourceClass::SharedRegion,
        labels: Vec::new(),
    };
    let served = ctx
        .serve(Serve {
            path: path.iter().map(|segment| segment.to_string()).collect(),
            target,
            default: false,
        })
        .await;
    match served {
        Ok(uri) => info!("serve-decl: unexpectedly served {uri}"),
        Err(error) => info!("serve-decl: refused ({error})"),
    }

    let named = format!(
        "{}{}/{}",
        selium_abi::uri::SEL_PREFIX,
        tenant,
        path.join("/")
    );
    let canonical =
        selium_abi::uri::resource_uri(&tenant, ResourceClass::SharedRegion, resource_id);
    for uri in [named, canonical] {
        info!("serve-decl: checking {uri}");
        let outcome = match ctx.lookup(&uri).await {
            Ok(Some(_)) => "present",
            Ok(None) => "absent",
            Err(error) => {
                info!("serve-decl: lookup failed for {uri}: {error}");
                "unknown"
            }
        };
        info!("serve-decl: route {uri} {outcome}");
    }
    // Stay resident so the test can drain every line: a guest that returns
    // immediately may exit before its log channel is read.
    park().await
}

/// Registers `uri` and then crashes, so revocation must run on the failure
/// path rather than through a graceful stop.
#[expect(
    clippy::panic,
    reason = "the deliberate crash is the point of this fixture role"
)]
async fn trap_after_register(ctx: &mut Context, uri: &str) -> anyhow::Result<()> {
    let channel = Channel::create(PROBE_CHANNEL_CAPACITY, ChannelBackpressure::Park)
        .context("crash: create channel")?;
    let resource_id = channel.region_id();
    match request_register(ctx, uri, region_target(uri, resource_id), false).await? {
        DiscoveryResponse::Registered => info!("crash: registered {uri} {resource_id}"),
        other => bail!("crash: registration was not admitted: {other:?}"),
    }
    // Park once so the guest is admitted by bootstrap (which runs the
    // entrypoint to its first park), then trap on the next wake. A trap during
    // the initial poll would be reported as a bootstrap failure, exercising the
    // wrong path: this fixture exists to drive the runtime's *crash* cleanup.
    let now = selium_guest::Instant::now().context("crash: clock")?;
    let deadline = now
        .checked_add(std::time::Duration::from_secs(5))
        .unwrap_or(selium_guest::Instant::MAX);
    selium_guest::Timer::new(deadline).await?;
    panic!("failure-injection: deliberate guest crash after registering {uri}");
}

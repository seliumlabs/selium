use selium_abi::uri::{self, DomainTable};
use selium_service::{DiscoveryRequest, DiscoveryResponse, ResourceTarget};
use selium_shm::rpc::{self, OwnedRpcClient};

use crate::{GuestError, resource::ResourceSender};

/// RPC ring capacity for discovery replies.
const RPC_REP_CAPACITY: u64 = 4096;
/// RPC ring capacity for discovery requests.
const RPC_REQ_CAPACITY: u64 = 4096;

/// Declaration of a named service route: a path plus the target to map it to,
/// with an optional root-service designation for apex aliasing.
#[derive(Debug, Clone)]
pub struct Serve {
    /// The internal path segments, e.g. `["bridge"]` or `["http", "prod"]`.
    /// Every segment must project to a DNS-safe wire label.
    pub path: Vec<String>,
    /// The resource the route maps to (typically the guest's own listener
    /// queue).
    pub target: ResourceTarget,
    /// Designate this path as the tenant's root service, so the bare domain
    /// (apex) resolves to it.
    pub default: bool,
}

/// Guest context injected by the runtime during bootstrap.
///
/// Provides a pre-connected discovery client for URI resolution via RPC
/// over shared-memory ring buffers.
pub struct Context {
    /// The raw discovery host-queue handle the context was built from. Kept
    /// so a guest can hand the handle onward (e.g. to a spawned child) after
    /// constructing a [`Context`] through the entrypoint macro.
    handle: u64,
    client: OwnedRpcClient<DiscoveryRequest, DiscoveryResponse>,
}

impl Context {
    /// Returns a mutable reference to the pre-connected discovery RPC client.
    ///
    /// Use this to send custom discovery requests beyond the convenience
    /// `lookup()` method.
    pub fn discovery(&mut self) -> &mut OwnedRpcClient<DiscoveryRequest, DiscoveryResponse> {
        &mut self.client
    }

    /// Creates a Context from a raw discovery handle.
    ///
    /// Attaches a `ResourceSender` to the discovery host queue and creates
    /// an `RpcClient` for discovery requests.
    pub async fn from_raw(discovery_handle: u64) -> Result<Self, GuestError> {
        // Attach to the discovery host queue.
        let sender = ResourceSender::attach(discovery_handle)?;

        // Create RPC client for discovery.
        let client = rpc::connect(sender, RPC_REQ_CAPACITY, RPC_REP_CAPACITY)
            .await
            .map_err(|e| GuestError::Host(format!("create RPC client: {e}")))?;

        Ok(Self {
            handle: discovery_handle,
            client,
        })
    }

    /// Returns the raw discovery host-queue handle this context was built
    /// from. Useful when a guest needs to forward the handle onward (e.g. to
    /// a spawned child via [`crate::Process::start`]) after receiving its
    /// [`Context`] through the entrypoint macro.
    pub fn raw_handle(&self) -> u64 {
        self.handle
    }

    /// Resolves a URI (typed, named-service route, leaf alias, or external
    /// wire name) to a resource via the discovery service.
    ///
    /// Convenience method that delegates to `self.discovery().request()`.
    /// A successful resolve also gives the caller an authorisation basis for
    /// `HostQueueAttach` on the returned queue: the discovery service records
    /// the resolved id with the runtime on the caller's behalf.
    pub async fn lookup(&mut self, uri: &str) -> Result<Option<ResourceTarget>, GuestError> {
        let request = DiscoveryRequest::Resolve(uri.to_string());

        let response = self
            .discovery()
            .request(request)
            .await
            .map_err(|e| GuestError::Host(format!("discovery request: {e}")))?;

        match response {
            DiscoveryResponse::Found(target) => Ok(Some(target)),
            DiscoveryResponse::NotFound => Ok(None),
            DiscoveryResponse::Registered
            | DiscoveryResponse::Revoked
            | DiscoveryResponse::Forbidden
            | DiscoveryResponse::Resolved(_)
            | DiscoveryResponse::Domains(_) => Err(GuestError::Host(
                "unexpected discovery response variant".to_string(),
            )),
        }
    }

    /// Fetches the advisory domain→tenant table from the discovery service,
    /// for local wire-name resolution (see
    /// [`uri::resolve_wire_name`](selium_abi::uri::resolve_wire_name)).
    pub async fn load_domains(&mut self) -> Result<DomainTable, GuestError> {
        let response = self
            .discovery()
            .request(DiscoveryRequest::ListDomains)
            .await
            .map_err(|e| GuestError::Host(format!("discovery domain list: {e}")))?;

        match response {
            DiscoveryResponse::Domains(entries) => {
                let mut table = DomainTable::new();
                for entry in entries {
                    table.seed(entry.domain, entry.tenant);
                }
                Ok(table)
            }
            other => Err(GuestError::Host(format!(
                "unexpected discovery response: {other:?}"
            ))),
        }
    }

    /// Registers a URI→target mapping in the discovery service.
    ///
    /// Convenience method that delegates to `self.discovery().request()`.
    /// Returns `Err(GuestError::Host("registration forbidden"))` if the discovery
    /// service rejects the registration.
    pub async fn register(&mut self, uri: &str, target: ResourceTarget) -> Result<(), GuestError> {
        self.register_inner(uri, target, false).await
    }

    /// Registers a named-service route from the guest's own declaration,
    /// deriving both the internal path (`sel://<tenant>/<path…>`) and its wire
    /// name (`<reversed path>.<tenant>`, or under an owned domain) from the one
    /// [`Serve`] value. The guest creates its own resource; the runtime no
    /// longer provisions routes on a guest's behalf.
    ///
    /// Registration in the root/system tenant is permitted only when the guest
    /// holds the system-registration capability (enforced by discovery).
    pub async fn serve(&mut self, serve: Serve) -> Result<String, GuestError> {
        // A named service must project to a wire name: reject typed-resource
        // paths and non-DNS-safe segments up front.
        let joined = serve.path.join("/");
        if uri::labels_from_path(&joined).is_none() {
            return Err(GuestError::Host(format!(
                "serve path {joined:?} cannot project to a wire name"
            )));
        }
        let (_, tenant) = crate::self_info()?;
        let uri = format!(
            "{}{}/{}",
            uri::SEL_PREFIX,
            tenant.unwrap_or_default(),
            joined
        );
        // The route's registration key is the derived internal path; pin it
        // onto the target so discovery registers under that URI.
        let mut target = serve.target;
        target.uri = uri.clone();
        self.register_inner(&uri, target, serve.default).await?;
        Ok(uri)
    }

    async fn register_inner(
        &mut self,
        uri: &str,
        target: ResourceTarget,
        root_service: bool,
    ) -> Result<(), GuestError> {
        let request = DiscoveryRequest::Register {
            uri: uri.to_string(),
            target,
            // Guests register on their own behalf; the runtime supplies the
            // Tier-1 owner over the discovery feed.
            owner: None,
            root_service,
        };

        let response = self
            .discovery()
            .request(request)
            .await
            .map_err(|e| GuestError::Host(format!("discovery register: {e}")))?;

        match response {
            DiscoveryResponse::Registered => Ok(()),
            DiscoveryResponse::Forbidden => Err(GuestError::Host(
                "registration forbidden: process does not own the route or lacks the \
                 system-registration capability"
                    .to_string(),
            )),
            other => Err(GuestError::Host(format!(
                "unexpected discovery response: {other:?}"
            ))),
        }
    }

    /// Revokes a URI→target mapping in the discovery service.
    ///
    /// Convenience method that delegates to `self.discovery().request()`.
    pub async fn revoke(&mut self, uri: &str) -> Result<(), GuestError> {
        let request = DiscoveryRequest::Revoke {
            uri: uri.to_string(),
        };

        let response = self
            .discovery()
            .request(request)
            .await
            .map_err(|e| GuestError::Host(format!("discovery revoke: {e}")))?;

        match response {
            DiscoveryResponse::Revoked => Ok(()),
            other => Err(GuestError::Host(format!(
                "unexpected discovery response: {other:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn from_raw_with_invalid_handle_fails() {
        let result = Context::from_raw(0).await;
        assert!(result.is_err());
        // In native mode, ResourceSender::attach(0) fails because there's
        // no host queue infrastructure.
    }

    #[test]
    fn serve_rejects_non_projectable_paths() {
        // Typed resource paths and non-DNS-safe segments cannot project.
        assert!(uri::labels_from_path("region/7").is_none());
        assert!(uri::labels_from_path("Foo").is_none());
        assert_eq!(
            uri::labels_from_path("http/prod"),
            Some(vec!["prod".to_string(), "http".to_string()])
        );
    }
}

//! Selium URI grammar, wire-name projection, and matching.
//!
//! Selium has one canonical address space rooted at `sel://<tenant>/<segments…>`:
//!
//! - `sel://<tenant>/<type>/<id>` — the internal typed schema. `<tenant>` is
//!   the URI authority (empty for the root/system tenant), `<type>` is a
//!   lowercased [`ResourceClass`] segment (`proc`, `region`, `queue`, …), and
//!   `<id>` is the resource's numeric identity. A non-class path
//!   (`sel://<tenant>/<name…>`) names a leaf service.
//! - External wire names are projections of that space, not a second
//!   namespace: a service's wire name is its path reversed, joined with dots,
//!   under a tenant-owned domain (`sel://acme/bridge` ↔ `bridge.acme`, or
//!   `bridge.example.com` when the advisory domain table maps
//!   `example.com -> acme`).
//!
//! The root tenant (`sel:///…`) is reserved: a guest registers inside it only
//! with the system-registration capability.
//!
//! This module is the single source of truth for these rules, shared by the
//! runtime (URI generation), the discovery guest (validation and unified wire
//! resolution), and the connectors (server-name resolution).

use std::collections::BTreeMap;

use super::ResourceClass;

/// The scheme of internal `sel` URIs.
pub const SEL_PREFIX: &str = "sel://";

/// Advisory domain-to-tenant mapping, provisioned out-of-band (like client
/// certificates). Used for routing and for scoping which tenant may register
/// names under a domain. It is **not** an authentication source: identity
/// still comes from the mTLS certificate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainTable {
    entries: BTreeMap<String, String>,
}

impl DomainTable {
    /// Creates an empty domain table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds a `domain -> tenant` entry. The domain is normalised
    /// (lowercased, trailing dot stripped).
    pub fn seed(&mut self, domain: impl Into<String>, tenant: impl Into<String>) {
        self.entries
            .insert(normalize_host(&domain.into()), tenant.into());
    }

    /// Returns the tenant mapped for an exact domain, if provisioned.
    pub fn get(&self, domain: &str) -> Option<&str> {
        self.entries.get(domain).map(String::as_str)
    }

    /// Returns whether no domains are provisioned.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the provisioned entries as a list of `(domain, tenant)` pairs.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(d, t)| (d.as_str(), t.as_str()))
    }
}

/// Returns whether `segment` names a resource class (a reserved type segment).
pub fn is_class_segment(segment: &str) -> bool {
    ResourceClass::from_uri_segment(segment).is_some()
}

/// Returns whether a path segment is a valid DNS label: 1–63 octets of ASCII
/// lowercase letters, digits, and hyphens, with no leading or trailing hyphen.
///
/// Wire names are DNS hostnames, so every projected segment must be a valid
/// label. The reversal is a convention enforced only in
/// [`resolve_wire_name`], but the projection helpers reject non-DNS-safe
/// segments up front so a service can never register a path that cannot be
/// projected to a wire name.
pub fn is_dns_safe_label(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 63
        && !segment.starts_with('-')
        && !segment.ends_with('-')
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Returns whether `uri` addresses the root/system tenant (`sel:///…`).
pub fn is_root_uri(uri: &str) -> bool {
    matches!(parse_sel(uri), Some((tenant, _)) if tenant.is_empty())
}

/// Projects a named-service path into reversed DNS labels.
///
/// `["http", "prod"]` → `["prod", "http"]`. The projection applies only to
/// named services: a class segment (a typed resource path such as
/// `region/<id>`) or any non-DNS-safe segment yields `None`, so resource
/// identity URIs never project to external wire names.
pub fn labels_from_path(path: &str) -> Option<Vec<String>> {
    let path = path.trim_matches('/');
    if path.is_empty() {
        return None;
    }
    let mut labels = Vec::new();
    for segment in path.split('/').rev() {
        if is_class_segment(segment) || !is_dns_safe_label(segment) {
            return None;
        }
        labels.push(segment.to_string());
    }
    Some(labels)
}

/// Normalises a host/authority value: lowercased, trailing dot stripped, and
/// a numeric `:port` suffix removed.
pub fn normalize_host(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    if let Some((name, port)) = host.rsplit_once(':')
        && port.chars().all(|c| c.is_ascii_digit())
        && !name.is_empty()
    {
        return name.to_string();
    }
    host.to_string()
}

/// Parses a leaf alias `sel://<tenant>/<name>` into `(tenant, name)`.
/// A class noun is reserved, so a name shadowing a type segment is rejected.
pub fn parse_alias(uri: &str) -> Option<(&str, &str)> {
    let (tenant, path) = parse_sel(uri)?;
    if path.is_empty() || path.contains('/') {
        return None;
    }
    if is_class_segment(path) {
        return None;
    }
    Some((tenant, path))
}

/// Parses a `sel://` URI into its `(tenant, path)` components.
///
/// `tenant` is the authority (empty for the root/system tenant); `path` is
/// the remainder with leading and trailing `/` stripped (it may be empty or
/// contain `/`-separated segments). Returns `None` for external names and
/// other non-`sel` URIs.
pub fn parse_sel(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix(SEL_PREFIX)?;
    let (tenant, path) = rest.split_once('/').unwrap_or((rest, ""));
    Some((tenant, path.trim_matches('/')))
}

/// Parses a typed internal URI `sel://<tenant>/<type>/<id>` into
/// `(tenant, class, id)`. Returns `None` for aliases, root well-known paths,
/// and external names.
pub fn parse_typed(uri: &str) -> Option<(&str, ResourceClass, u64)> {
    let (tenant, path) = parse_sel(uri)?;
    let (class_seg, id_seg) = path.split_once('/')?;
    if id_seg.is_empty() || id_seg.contains('/') {
        return None;
    }
    let class = ResourceClass::from_uri_segment(class_seg)?;
    let id = id_seg.parse::<u64>().ok()?;
    Some((tenant, class, id))
}

/// Joins reversed DNS labels back into an internal path: `["prod", "http"]` →
/// `"http/prod"`. Returns `None` when any label is not DNS-safe.
pub fn path_from_labels(labels: &[&str]) -> Option<String> {
    if labels.is_empty() {
        return None;
    }
    let mut segments = Vec::with_capacity(labels.len());
    for label in labels.iter().rev() {
        if !is_dns_safe_label(label) {
            return None;
        }
        segments.push(*label);
    }
    Some(segments.join("/"))
}

/// Resolves an external wire name into its `(tenant, path)` components.
///
/// The name is host-normalised (lowercased, trailing dot and port stripped),
/// then the longest registered domain suffix is stripped to derive the
/// tenant; without a table entry, the final label is the tenant (the
/// synthetic `<tenant>` label, resolvable cluster-internally with no table
/// entry). The remaining labels are reversed into the internal path and must
/// be DNS-safe.
///
/// - `bridge.acme` → `("acme", ["bridge"])`
/// - `bridge.example.com` with `example.com -> acme` → `("acme", ["bridge"])`
/// - `prod.http.example.com` → `("acme", ["http", "prod"])`
/// - `example.com` with `example.com -> acme` → `("acme", [])` (apex)
/// - `localhost` → `("", ["localhost"])` (single synthetic label, root namespace)
pub fn resolve_wire_name(name: &str, domains: &DomainTable) -> Option<(String, Vec<String>)> {
    let name = normalize_host(name);
    if name.is_empty() {
        return None;
    }
    let labels: Vec<&str> = name.split('.').collect();

    // Longest registered-domain suffix: iterate label-suffix lengths from
    // longest to shortest, keeping the first (longest) match.
    let domain_match = (1..=labels.len()).rev().find_map(|suffix_len| {
        let start = labels.len() - suffix_len;
        let domain = labels.get(start..)?.join(".");
        domains
            .get(&domain)
            .map(|tenant| (tenant.to_string(), start))
    });

    let (tenant, remaining_len) = match domain_match {
        Some(matched) => matched,
        None => match labels.split_last() {
            // Synthetic fallback: the final label is the tenant.
            Some((tenant, rest)) if !rest.is_empty() => (tenant.to_string(), labels.len() - 1),
            // A single label lives under the root/system namespace.
            _ => (String::new(), 1),
        },
    };

    let remaining = labels.get(..remaining_len).unwrap_or_default();
    if remaining.is_empty() {
        // Apex: the bare domain maps to the tenant's root service.
        return Some((tenant, Vec::new()));
    }
    let mut path = Vec::with_capacity(remaining.len());
    for label in remaining.iter().rev() {
        if !is_dns_safe_label(label) {
            return None;
        }
        path.push((*label).to_string());
    }
    Some((tenant, path))
}

/// Builds a typed resource URI: `sel://<tenant>/<type>/<id>`.
pub fn resource_uri(tenant: &str, class: ResourceClass, id: u64) -> String {
    format!("{SEL_PREFIX}{tenant}/{}/{id}", class.uri_segment())
}

/// Returns whether a `sel` path is a wildcard enumeration (`…/*`), and the
/// non-wildcard prefix of the path (everything before the `/*`).
pub fn wildcard_prefix(path: &str) -> Option<&str> {
    let stripped = path.trim_end_matches('/');
    let prefix = stripped.strip_suffix("*")?;
    let prefix = prefix.trim_end_matches('/');
    Some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sel_extracts_tenant_and_path() {
        assert_eq!(parse_sel("sel://tenant/app"), Some(("tenant", "app")));
        assert_eq!(parse_sel("sel://acme/region/7"), Some(("acme", "region/7")));
        assert_eq!(parse_sel("sel:///dns/resolve"), Some(("", "dns/resolve")));
        assert_eq!(parse_sel("sel:///proc/1"), Some(("", "proc/1")));
        assert_eq!(parse_sel("https://example.com/"), None);
        assert_eq!(parse_sel("example.com"), None);
    }

    #[test]
    fn root_uri_detection() {
        assert!(is_root_uri("sel:///dns/resolve"));
        assert!(is_root_uri("sel:///proc/1"));
        assert!(!is_root_uri("sel://acme/proc/1"));
        assert!(!is_root_uri("https://example.com/"));
    }

    #[test]
    fn resource_uri_uses_typed_segment() {
        assert_eq!(
            resource_uri("acme", ResourceClass::SharedRegion, 7),
            "sel://acme/region/7"
        );
        assert_eq!(
            resource_uri("acme", ResourceClass::Process, 42),
            "sel://acme/proc/42"
        );
        assert_eq!(
            resource_uri("acme", ResourceClass::HostQueue, 9),
            "sel://acme/queue/9"
        );
        assert_eq!(
            resource_uri("", ResourceClass::HostQueue, 9),
            "sel:///queue/9"
        );
    }

    #[test]
    fn parse_typed_recognises_typed_uris() {
        assert_eq!(
            parse_typed("sel://acme/region/7"),
            Some(("acme", ResourceClass::SharedRegion, 7))
        );
        assert_eq!(
            parse_typed("sel://acme/proc/42"),
            Some(("acme", ResourceClass::Process, 42))
        );
        assert_eq!(
            parse_typed("sel:///queue/9"),
            Some(("", ResourceClass::HostQueue, 9))
        );
        assert_eq!(parse_typed("sel://acme/proxy"), None);
        assert_eq!(parse_typed("sel:///dns/resolve"), None);
        assert_eq!(parse_typed("https://example.com/"), None);
    }

    #[test]
    fn parse_alias_recognises_leaf_names() {
        assert_eq!(parse_alias("sel://acme/proxy"), Some(("acme", "proxy")));
        assert_eq!(parse_alias("sel:///discovery"), Some(("", "discovery")));
        // Class nouns are reserved: an alias cannot shadow a type segment.
        assert_eq!(parse_alias("sel://acme/region"), None);
        assert_eq!(parse_alias("sel://acme/proc"), None);
        assert_eq!(parse_alias("sel://acme/region/7"), None);
        assert_eq!(parse_alias("https://example.com/"), None);
    }

    #[test]
    fn class_segment_detection() {
        assert!(is_class_segment("proc"));
        assert!(is_class_segment("region"));
        assert!(is_class_segment("queue"));
        assert!(!is_class_segment("proxy"));
        assert!(!is_class_segment(""));
    }

    #[test]
    fn wildcard_prefix_strips_star() {
        assert_eq!(wildcard_prefix("region/*"), Some("region"));
        assert_eq!(wildcard_prefix("region/*/"), Some("region"));
        assert_eq!(wildcard_prefix("*"), Some(""));
        assert_eq!(wildcard_prefix("region/7"), None);
    }

    #[test]
    fn normalize_host_strips_case_dots_and_ports() {
        assert_eq!(normalize_host("Example.COM."), "example.com");
        assert_eq!(normalize_host("example.com:443"), "example.com");
        assert_eq!(normalize_host("bridge"), "bridge");
    }

    #[test]
    fn labels_from_path_reverses_and_rejects_unsafe() {
        assert_eq!(labels_from_path("bridge"), Some(vec!["bridge".to_string()]));
        assert_eq!(
            labels_from_path("http/prod"),
            Some(vec!["prod".to_string(), "http".to_string()])
        );
        // Typed resource paths (class nouns) never project.
        assert_eq!(labels_from_path("region/7"), None);
        // Non-DNS-safe segments are rejected.
        assert_eq!(labels_from_path("Foo"), None);
        assert_eq!(labels_from_path("foo_bar"), None);
        assert_eq!(labels_from_path(""), None);
    }

    #[test]
    fn path_from_labels_inverts_labels_from_path() {
        assert_eq!(path_from_labels(&["bridge"]), Some("bridge".to_string()));
        assert_eq!(
            path_from_labels(&["prod", "http"]),
            Some("http/prod".to_string())
        );
        assert_eq!(path_from_labels(&["Foo"]), None);
        assert_eq!(path_from_labels(&[]), None);
    }

    #[test]
    fn dns_safe_label_rules() {
        assert!(is_dns_safe_label("bridge"));
        assert!(is_dns_safe_label("br1dge-2"));
        assert!(!is_dns_safe_label(""));
        assert!(!is_dns_safe_label("-lead"));
        assert!(!is_dns_safe_label("trail-"));
        assert!(!is_dns_safe_label("UPPER"));
        assert!(!is_dns_safe_label("under_score"));
    }

    #[test]
    fn resolve_wire_name_uses_synthetic_tenant_label() {
        let domains = DomainTable::new();
        assert_eq!(
            resolve_wire_name("bridge.acme", &domains),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
        // A single synthetic label lives under the root/system namespace.
        assert_eq!(
            resolve_wire_name("localhost", &domains),
            Some((String::new(), vec!["localhost".to_string()]))
        );
        assert_eq!(resolve_wire_name("", &domains), None);
    }

    /// Root-namespace (platform-tenant) wire names project to the bare
    /// reversed path with no tenant domain suffix: `bridge` → `sel:///bridge`,
    /// `control` → `sel:///control`.
    #[test]
    fn resolve_wire_name_projects_bare_root_names() {
        let domains = DomainTable::new();
        assert_eq!(
            resolve_wire_name("bridge", &domains),
            Some((String::new(), vec!["bridge".to_string()]))
        );
        assert_eq!(
            resolve_wire_name("control", &domains),
            Some((String::new(), vec!["control".to_string()]))
        );
        // A registered domain table does not change the bare-name projection.
        let mut with_domains = DomainTable::new();
        with_domains.seed("example.com", "acme");
        assert_eq!(
            resolve_wire_name("bridge", &with_domains),
            Some((String::new(), vec!["bridge".to_string()]))
        );
        // The tenant-scoped shape still projects alongside the bare case.
        assert_eq!(
            resolve_wire_name("bridge.acme", &with_domains),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
    }

    #[test]
    fn resolve_wire_name_strips_registered_domains() {
        let mut domains = DomainTable::new();
        domains.seed("example.com", "acme");

        assert_eq!(
            resolve_wire_name("bridge.example.com", &domains),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
        // Deeper paths reverse in full.
        assert_eq!(
            resolve_wire_name("prod.http.example.com", &domains),
            Some((
                "acme".to_string(),
                vec!["http".to_string(), "prod".to_string()]
            ))
        );
        // The synthetic label still works when no domain matches.
        assert_eq!(
            resolve_wire_name("bridge.acme", &domains),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
    }

    #[test]
    fn resolve_wire_name_prefers_longest_domain_match() {
        let mut domains = DomainTable::new();
        domains.seed("example.com", "acme");
        domains.seed("api.example.com", "beta");

        assert_eq!(
            resolve_wire_name("svc.api.example.com", &domains),
            Some(("beta".to_string(), vec!["svc".to_string()]))
        );
    }

    #[test]
    fn resolve_wire_name_apex_has_empty_path() {
        let mut domains = DomainTable::new();
        domains.seed("example.com", "acme");
        assert_eq!(
            resolve_wire_name("example.com", &domains),
            Some(("acme".to_string(), Vec::new()))
        );
        assert_eq!(
            resolve_wire_name("Example.COM.", &domains),
            Some(("acme".to_string(), Vec::new()))
        );
    }

    #[test]
    fn resolve_wire_name_normalises_host_ports_and_case() {
        let mut domains = DomainTable::new();
        domains.seed("example.com", "acme");
        assert_eq!(
            resolve_wire_name("bridge.example.com:443", &domains),
            Some(("acme".to_string(), vec!["bridge".to_string()]))
        );
    }
}

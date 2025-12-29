//! Selium URI parsing and Flatbuffers conversion.
//!
//! Selium uses `sel://` URIs to identify tenants, applications, services, and endpoints.
//!
//! # Examples
//! ```
//! use selium_userland::uri::Uri;
//!
//! fn main() -> Result<(), selium_userland::uri::UriError> {
//!     let uri = Uri::parse("sel://tenant/app.app/service:ep")?;
//!     assert_eq!(uri.tenant(), "tenant");
//!     assert_eq!(uri.application(), Some("app.app"));
//!     assert_eq!(uri.service(), Some("service"));
//!     assert_eq!(uri.endpoint(), Some("ep"));
//!     Ok(())
//! }
//! ```

use std::hash::{Hash, Hasher};
use std::{fmt, fmt::Write as _};

use flatbuffers::{self, FlatBufferBuilder, WIPOffset};
use thiserror::Error;
use url::{ParseError, Url};

use crate::fbs::selium::uri as fb;

type Result<T, E = UriError> = std::result::Result<T, E>;

/// Parsed Selium URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uri {
    tenant: String,
    path: Option<Vec<String>>,
    application: Option<String>,
    service: Option<String>,
    endpoint: Option<String>,
}

/// Errors returned when parsing a Selium URI.
#[derive(Error, Debug)]
pub enum UriError {
    /// Parsing the URI string failed.
    #[error("Unable to parse string into URI")]
    BadUri(#[from] ParseError),
    /// The URL scheme was not `sel`.
    #[error("URI must use Selium scheme: \"sel://\"")]
    InvalidScheme,
    /// A tenant component was not present.
    #[error("URI did not contain a tenant")]
    MissingTenant,
    /// The path segments could not be interpreted as a Selium URI.
    #[error("URI path segments invalid")]
    InvalidPath,
}

/// Errors returned when decoding a Flatbuffers `Uri`.
#[derive(Error, Debug)]
pub enum UriFlatbufferError {
    /// Flatbuffers verification failed.
    #[error("Flatbuffer verification failed: {0:?}")]
    Invalid(flatbuffers::InvalidFlatbuffer),
}

impl From<flatbuffers::InvalidFlatbuffer> for UriFlatbufferError {
    fn from(value: flatbuffers::InvalidFlatbuffer) -> Self {
        Self::Invalid(value)
    }
}

impl Uri {
    /// Parse a Selium URI from a string or an existing `Url`.
    pub fn parse<U>(uri: U) -> Result<Self>
    where
        U: TryInto<Url, Error = ParseError>,
    {
        let url = uri.try_into().map_err(UriError::BadUri)?;
        Self::parse_url(url)
    }

    /// Parse a Selium URI from a pre-parsed `Url`.
    pub fn parse_url(uri: Url) -> Result<Self> {
        if uri.scheme() != "sel" {
            return Err(UriError::InvalidScheme);
        }

        let mut this = Self {
            tenant: uri.domain().ok_or(UriError::MissingTenant)?.into(),
            path: None,
            application: None,
            service: None,
            endpoint: None,
        };

        let has_trailing_slash = uri.path().ends_with('/');

        if let Some(segments_iter) = uri.path_segments() {
            let segments: Vec<String> = segments_iter
                .filter(|segment| !segment.is_empty())
                .map(|segment| segment.to_string())
                .collect();

            if segments.is_empty() {
                return Ok(this);
            }

            populate_segments(&segments, has_trailing_slash, &mut this)?;
        }

        Ok(this)
    }

    /// Tenant extracted from the URI.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Optional path segments following the tenant.
    pub fn path(&self) -> Option<&[String]> {
        self.path.as_deref()
    }

    /// Optional application name.
    pub fn application(&self) -> Option<&str> {
        self.application.as_deref()
    }

    /// Optional service name.
    pub fn service(&self) -> Option<&str> {
        self.service.as_deref()
    }

    /// Optional endpoint name.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// Tests whether one URI has a broader scope than another.
    ///
    /// Examples:
    /// - `sel://a/b.app/service` contains `sel://a/b.app/service:endpoint`
    /// - `sel://a` contains `sel://a`
    /// - `sel://a/b` does not contain `sel://a`
    pub fn contains(&self, other: &Self) -> bool {
        if self.tenant != other.tenant {
            return false;
        }

        if let Some(self_path) = &self.path {
            let Some(other_path) = &other.path else {
                return false;
            };

            if other_path.len() < self_path.len() {
                return false;
            }

            if !self_path
                .iter()
                .zip(other_path.iter())
                .all(|(self_segment, other_segment)| self_segment == other_segment)
            {
                return false;
            }
        }

        match (&self.application, &other.application) {
            (Some(self_app), Some(other_app)) if self_app != other_app => return false,
            (Some(_), None) => return false,
            _ => {}
        }

        match (&self.service, &other.service) {
            (Some(self_service), Some(other_service)) if self_service != other_service => {
                return false;
            }
            (Some(_), None) => return false,
            _ => {}
        }

        match (&self.endpoint, &other.endpoint) {
            (Some(self_endpoint), Some(other_endpoint)) if self_endpoint != other_endpoint => {
                return false;
            }
            (Some(_), None) => return false,
            _ => {}
        }

        true
    }

    /// Serialises this URI into a Flatbuffer byte vector.
    pub fn to_flatbuffer_bytes(&self) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::new();
        let offset = self.write_flatbuffer(&mut builder);
        builder.finish(offset, None);
        builder.finished_data().to_vec()
    }

    /// Deserialises a URI from a Flatbuffer payload.
    pub fn from_flatbuffer_bytes(bytes: &[u8]) -> Result<Self, UriFlatbufferError> {
        let fb_uri = fb::root_as_uri(bytes)?;
        Ok(Self::from_flatbuffer_table(fb_uri))
    }

    pub(crate) fn write_flatbuffer<'bldr, A: flatbuffers::Allocator + 'bldr>(
        &self,
        builder: &mut FlatBufferBuilder<'bldr, A>,
    ) -> WIPOffset<fb::Uri<'bldr>> {
        let tenant = builder.create_string(&self.tenant);
        let path = self.path.as_ref().map(|segments| {
            let offsets: Vec<_> = segments
                .iter()
                .map(|segment| builder.create_string(segment))
                .collect();
            builder.create_vector(&offsets)
        });
        let application = self
            .application
            .as_ref()
            .map(|value| builder.create_string(value));
        let service = self
            .service
            .as_ref()
            .map(|value| builder.create_string(value));
        let endpoint = self
            .endpoint
            .as_ref()
            .map(|value| builder.create_string(value));

        fb::Uri::create(
            builder,
            &fb::UriArgs {
                tenant: Some(tenant),
                path,
                application,
                service,
                endpoint,
            },
        )
    }

    pub(crate) fn from_flatbuffer_table(table: fb::Uri<'_>) -> Self {
        let path = table.path().map(|vector| {
            vector
                .iter()
                .map(|segment| segment.to_string())
                .collect::<Vec<_>>()
        });

        Self {
            tenant: table.tenant().to_string(),
            path,
            application: table.application().map(|value| value.to_string()),
            service: table.service().map(|value| value.to_string()),
            endpoint: table.endpoint().map(|value| value.to_string()),
        }
    }
}

impl fmt::Display for Uri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sel://")?;
        f.write_str(&self.tenant)?;

        if let Some(path) = &self.path {
            for segment in path {
                f.write_char('/')?;
                f.write_str(segment)?;
            }
        }

        if let Some(application) = &self.application {
            f.write_char('/')?;
            f.write_str(application)?;
        }

        if let Some(service) = &self.service {
            f.write_char('/')?;
            f.write_str(service)?;
        }

        if let Some(endpoint) = &self.endpoint {
            f.write_char(':')?;
            f.write_str(endpoint)?;
        }

        Ok(())
    }
}

impl<'a> TryFrom<&'a str> for Uri {
    type Error = UriError;

    fn try_from(value: &'a str) -> std::result::Result<Self, Self::Error> {
        Uri::parse(value)
    }
}

impl TryFrom<Url> for Uri {
    type Error = UriError;

    fn try_from(value: Url) -> std::result::Result<Self, Self::Error> {
        Uri::parse_url(value)
    }
}

impl Hash for Uri {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.to_string().hash(state)
    }
}

fn parse_service_segment(segment: String) -> Result<(String, Option<String>)> {
    if let Some((service, endpoint)) = segment.split_once(':') {
        if service.is_empty() || endpoint.is_empty() {
            return Err(UriError::InvalidPath);
        }
        return Ok((service.to_string(), Some(endpoint.to_string())));
    }

    Ok((segment, None))
}

fn populate_segments(segments: &[String], has_trailing_slash: bool, uri: &mut Uri) -> Result<()> {
    let mut cutoff = segments.len();

    if cutoff == 0 {
        return Ok(());
    }

    // Service (and optional endpoint) exist only when there is no trailing slash and a `.app`
    // segment immediately precedes the tail.
    if !has_trailing_slash && cutoff >= 2 && segments[cutoff - 2].ends_with(".app") {
        let raw_service = segments[cutoff - 1].clone();
        let (service, endpoint) = parse_service_segment(raw_service)?;
        uri.service = Some(service);
        uri.endpoint = endpoint;
        cutoff -= 1;
    }

    if cutoff > 0 && segments[cutoff - 1].ends_with(".app") {
        uri.application = Some(segments[cutoff - 1].clone());
        cutoff -= 1;
    }

    if cutoff > 0 {
        uri.path = Some(segments[..cutoff].to_vec());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tenant_only() {
        let uri = Uri::parse("sel://tenant").expect("parse");
        assert_eq!(uri.tenant, "tenant");
        assert!(uri.path.is_none());
        assert!(uri.application.is_none());
        assert!(uri.service.is_none());
        assert!(uri.endpoint.is_none());
    }

    #[test]
    fn parses_application_without_service() {
        let uri = Uri::parse("sel://tenant/app.app/").expect("parse app");
        assert_eq!(uri.tenant, "tenant");
        assert_eq!(uri.application.as_deref(), Some("app.app"));
        assert!(uri.service.is_none());
        assert!(uri.endpoint.is_none());
        assert!(uri.path.is_none());
    }

    #[test]
    fn parses_path_application_service_and_endpoint() {
        let uri = Uri::parse("sel://tenant/team/payments/orders.app/runner:rpc").expect("parse");
        assert_eq!(uri.tenant, "tenant");
        assert_eq!(
            uri.path,
            Some(vec!["team".to_string(), "payments".to_string()])
        );
        assert_eq!(uri.application.as_deref(), Some("orders.app"));
        assert_eq!(uri.service.as_deref(), Some("runner"));
        assert_eq!(uri.endpoint.as_deref(), Some("rpc"));
    }

    #[test]
    fn parses_service_without_endpoint() {
        let uri = Uri::parse("sel://tenant/app.app/service").expect("parse");
        assert_eq!(uri.application.as_deref(), Some("app.app"));
        assert_eq!(uri.service.as_deref(), Some("service"));
        assert!(uri.endpoint.is_none());
    }

    #[test]
    fn treats_segments_without_app_suffix_as_path() {
        let uri = Uri::parse("sel://tenant/team/service").expect("parse path");
        assert_eq!(
            uri.path,
            Some(vec!["team".to_string(), "service".to_string()])
        );
        assert!(uri.application.is_none());
        assert!(uri.service.is_none());
        assert!(uri.endpoint.is_none());
    }

    #[test]
    fn flatbuffer_roundtrip() {
        let uri =
            Uri::parse("sel://tenant/team/payments/orders.app/runner:rpc").expect("parse uri");
        let bytes = uri.to_flatbuffer_bytes();
        let decoded = Uri::from_flatbuffer_bytes(&bytes).expect("decode uri");

        assert_eq!(decoded.tenant(), uri.tenant());
        assert_eq!(decoded.application(), uri.application());
        assert_eq!(decoded.service(), uri.service());
        assert_eq!(decoded.endpoint(), uri.endpoint());
        assert_eq!(
            decoded.path().map(|segments| segments.to_vec()),
            uri.path().map(|segments| segments.to_vec())
        );
    }

    #[test]
    fn contains_endpoint_scope() {
        let broader = Uri::parse("sel://tenant/app.app/service").expect("parse uri");
        let narrower = Uri::parse("sel://tenant/app.app/service:endpoint").expect("parse uri");

        assert!(broader.contains(&narrower));
    }

    #[test]
    fn contains_equal_scope() {
        let uri = Uri::parse("sel://tenant").expect("parse uri");
        assert!(uri.contains(&uri));
    }

    #[test]
    fn contains_path_prefix() {
        let parent = Uri::parse("sel://tenant/team").expect("parse uri");
        let child = Uri::parse("sel://tenant/team/payments/orders.app/runner").expect("parse uri");

        assert!(parent.contains(&child));
    }

    #[test]
    fn does_not_contain_broader_scope() {
        let narrow = Uri::parse("sel://tenant/team").expect("parse uri");
        let broad = Uri::parse("sel://tenant").expect("parse uri");

        assert!(!narrow.contains(&broad));
    }

    #[test]
    fn contains_requires_matching_components() {
        let service_scope = Uri::parse("sel://tenant/app.app/service").expect("parse uri");
        let different_service = Uri::parse("sel://tenant/app.app/other").expect("parse uri");
        assert!(!service_scope.contains(&different_service));

        let endpoint_scope = Uri::parse("sel://tenant/app.app/service:rpc").expect("parse uri");
        let missing_endpoint = Uri::parse("sel://tenant/app.app/service").expect("parse uri");
        assert!(!endpoint_scope.contains(&missing_endpoint));

        let app_scope = Uri::parse("sel://tenant/app.app").expect("parse uri");
        let other_app = Uri::parse("sel://tenant/other.app").expect("parse uri");
        assert!(!app_scope.contains(&other_app));
    }
}

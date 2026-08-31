//! Secure URL canonicalization, origin scoping, address checks, and IDs.
//!
//! This module is deliberately independent of the fetcher.  It accepts a URL,
//! applies the same deterministic policy every time, and exposes canonical
//! values that later crawler and output modules can use as stable keys.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use url::{Host, Url};

pub use crate::config::QueryMode;

const HTTP_DEFAULT_PORT: u16 = 80;
const HTTPS_DEFAULT_PORT: u16 = 443;

/// A normalized HTTP(S) URL.
///
/// The URL has a lowercase IDNA host, no userinfo or fragment, no explicit
/// default port, a non-empty path, and either a dropped or unchanged query as
/// requested by [`QueryMode`].  Its path is safe to use as a crawler key: an
/// encoded slash, backslash, or encoded traversal segment is rejected.
#[derive(Debug, Clone)]
pub struct CanonicalUrl {
    url: Url,
    origin: Origin,
}

impl CanonicalUrl {
    /// Returns the canonical URL as a string slice.
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// Borrows the parsed URL for fetcher integrations.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Returns the effective origin of this URL.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Returns the stable SHA-256 page ID for this canonical URL.
    pub fn page_id(&self) -> String {
        page_id(self.as_str())
    }

    /// Returns a portable relative output path for this page.
    pub fn output_path(&self) -> PathBuf {
        output_path(self.as_str())
    }
}

impl fmt::Display for CanonicalUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl PartialEq for CanonicalUrl {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for CanonicalUrl {}

impl Hash for CanonicalUrl {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

/// The effective scheme, host, and port used for origin comparisons.
///
/// Default HTTP and HTTPS ports are materialized, so `https://host` and
/// `https://host:443` have the same origin while `http://host:443` does not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    /// Parses and canonicalizes an HTTP(S) URL, returning only its origin.
    pub fn parse(input: &str) -> Result<Self, UrlPolicyError> {
        canonicalize_url(input, QueryMode::Drop).map(|url| url.origin().clone())
    }

    /// Returns the lowercase scheme (`http` or `https`).
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Returns the ASCII IDNA host without IPv6 brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the effective port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// A URL policy rooted at one origin.
///
/// Policies are exact-origin by default.  Subdomain inclusion is opt-in and
/// uses DNS label boundaries rather than a string prefix.  Literal special or
/// private addresses are denied unless `allow_private_network` is explicitly
/// enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlPolicy {
    origin: Origin,
    query_mode: QueryMode,
    include_subdomains: bool,
    allow_private_network: bool,
}

impl UrlPolicy {
    /// Creates a strict exact-origin policy with dropped queries.
    pub fn new(start_url: &str) -> Result<Self, UrlPolicyError> {
        Self::with_options(start_url, QueryMode::Drop, false, false)
    }

    /// Creates an exact-origin policy with a selected query mode.
    pub fn with_query_mode(start_url: &str, query_mode: QueryMode) -> Result<Self, UrlPolicyError> {
        Self::with_options(start_url, query_mode, false, false)
    }

    /// Creates a policy with explicit scope, query, and private-network flags.
    pub fn with_options(
        start_url: &str,
        query_mode: QueryMode,
        include_subdomains: bool,
        allow_private_network: bool,
    ) -> Result<Self, UrlPolicyError> {
        let start = canonicalize_url_inner(start_url, query_mode, allow_private_network)?;
        Ok(Self {
            origin: start.origin().clone(),
            query_mode,
            include_subdomains,
            allow_private_network,
        })
    }

    /// Alias for [`UrlPolicy::with_options`] with a descriptive name.
    pub fn new_with_options(
        start_url: &str,
        query_mode: QueryMode,
        include_subdomains: bool,
        allow_private_network: bool,
    ) -> Result<Self, UrlPolicyError> {
        Self::with_options(
            start_url,
            query_mode,
            include_subdomains,
            allow_private_network,
        )
    }

    /// Returns the policy's root origin.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Returns the configured query behavior.
    pub fn query_mode(&self) -> QueryMode {
        self.query_mode
    }

    /// Returns whether DNS-label subdomains are included.
    pub fn include_subdomains(&self) -> bool {
        self.include_subdomains
    }

    /// Returns whether private/special literal targets are allowed.
    pub fn allow_private_network(&self) -> bool {
        self.allow_private_network
    }

    /// Canonicalizes a URL under this policy's query and address settings.
    pub fn canonicalize(&self, input: &str) -> Result<CanonicalUrl, UrlPolicyError> {
        canonicalize_url_inner(input, self.query_mode, self.allow_private_network)
    }

    /// Returns whether a URL is a valid, in-scope URL.
    pub fn is_in_scope(&self, input: &str) -> Result<bool, UrlPolicyError> {
        let url = self.canonicalize(input)?;
        Ok(self.contains(&url))
    }

    /// Returns whether an already canonicalized URL is in scope.
    pub fn contains(&self, url: &CanonicalUrl) -> bool {
        let candidate = url.origin();
        if candidate.scheme != self.origin.scheme || candidate.port != self.origin.port {
            return false;
        }
        if candidate.host == self.origin.host {
            return true;
        }
        if !self.include_subdomains || is_ip_host(&self.origin.host) {
            return false;
        }
        candidate
            .host
            .strip_suffix(&format!(".{}", self.origin.host))
            .is_some_and(|prefix| !prefix.is_empty())
    }
}

/// The reason an IP address is accepted or denied by the default policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpSafety {
    /// A globally routable-looking address not in a blocked special range.
    Public,
    /// An address in a private, local, reserved, transition, or special-use range.
    Special(&'static str),
}

/// Errors returned while parsing or applying the URL policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlPolicyError {
    /// The input contains whitespace, which is not accepted by this API.
    WhitespaceNotAllowed,
    /// The URL parser rejected the input.
    InvalidUrl { reason: String },
    /// A non-HTTP(S) scheme was supplied.
    UnsupportedScheme { scheme: String },
    /// HTTP(S) requires a host.
    MissingHost,
    /// Credentials in a URL are never accepted.
    UserinfoNotAllowed,
    /// The host was empty after normalization.
    InvalidHost,
    /// A literal host resolves to a special/private address.
    UnsafeAddress { host: String, reason: &'static str },
    /// A path contains a percent-encoded slash or backslash.
    EncodedPathSeparator,
    /// A path contains an encoded `.` or `..` segment.
    EncodedPathTraversal,
    /// A raw backslash would be interpreted inconsistently by clients.
    BackslashInPath,
    /// The URL library could not apply a normalized host or port.
    NormalizationFailed { reason: String },
}

impl fmt::Display for UrlPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WhitespaceNotAllowed => formatter.write_str("URL must not contain whitespace"),
            Self::InvalidUrl { reason } => write!(formatter, "invalid URL: {reason}"),
            Self::UnsupportedScheme { scheme } => {
                write!(
                    formatter,
                    "unsupported URL scheme `{scheme}` (expected http or https)"
                )
            }
            Self::MissingHost => formatter.write_str("URL must contain a host"),
            Self::UserinfoNotAllowed => formatter.write_str("URL userinfo is not allowed"),
            Self::InvalidHost => formatter.write_str("URL must contain a non-empty host"),
            Self::UnsafeAddress { host, reason } => {
                write!(formatter, "unsafe destination `{host}`: {reason}")
            }
            Self::EncodedPathSeparator => {
                formatter.write_str("encoded path separators are not allowed")
            }
            Self::EncodedPathTraversal => {
                formatter.write_str("encoded path traversal is not allowed")
            }
            Self::BackslashInPath => formatter.write_str("backslashes in paths are not allowed"),
            Self::NormalizationFailed { reason } => {
                write!(formatter, "URL normalization failed: {reason}")
            }
        }
    }
}

impl std::error::Error for UrlPolicyError {}

/// Canonicalizes an HTTP(S) URL using the supplied query mode and the default
/// deny-private address policy.
pub fn canonicalize_url(
    input: &str,
    query_mode: QueryMode,
) -> Result<CanonicalUrl, UrlPolicyError> {
    canonicalize_url_inner(input, query_mode, false)
}

/// Returns the SHA-256 hex ID for a canonical URL string.
///
/// Callers should pass [`CanonicalUrl::as_str`], not an uncanonicalized URL.
/// The function intentionally hashes the exact bytes supplied so query
/// preservation remains collision-free and no query normalization is hidden.
pub fn page_id(canonical_url: &str) -> String {
    let digest = Sha256::digest(canonical_url.as_bytes());
    let mut result = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
    }
    result
}

/// Returns a deterministic, portable relative path for a canonical URL.
///
/// Hash-only names avoid traversal, platform-specific separators, Windows
/// reserved device names, and path-length dependence.  The path is always
/// `pages/<64 lowercase hex characters>.md`.
pub fn output_path(canonical_url: &str) -> PathBuf {
    PathBuf::from("pages").join(format!("{}.md", page_id(canonical_url)))
}

/// Canonicalizes a URL and returns its page ID in one operation.
pub fn page_id_for_url(input: &str, query_mode: QueryMode) -> Result<String, UrlPolicyError> {
    canonicalize_url(input, query_mode).map(|url| url.page_id())
}

/// Canonicalizes a URL and returns its deterministic output path.
pub fn output_path_for_url(input: &str, query_mode: QueryMode) -> Result<PathBuf, UrlPolicyError> {
    canonicalize_url(input, query_mode).map(|url| url.output_path())
}

/// Classifies an IP address against the default deny-private policy.
pub fn classify_ip(address: IpAddr) -> IpSafety {
    match address {
        IpAddr::V4(address) => classify_ipv4(address),
        IpAddr::V6(address) => {
            // Treat IPv4-compatible and IPv4-mapped addresses according to
            // their embedded IPv4 address; this closes the common SSRF bypass.
            if let Some(embedded) = address.to_ipv4() {
                return classify_ipv4(embedded);
            }
            classify_ipv6(address)
        }
    }
}

/// Returns true only for an address outside the blocked special-use ranges.
pub fn is_safe_ip(address: IpAddr) -> bool {
    matches!(classify_ip(address), IpSafety::Public)
}

fn canonicalize_url_inner(
    input: &str,
    query_mode: QueryMode,
    allow_private_network: bool,
) -> Result<CanonicalUrl, UrlPolicyError> {
    if input.chars().any(char::is_whitespace) {
        return Err(UrlPolicyError::WhitespaceNotAllowed);
    }
    if input.contains('\\') {
        return Err(UrlPolicyError::BackslashInPath);
    }
    if has_empty_authority(input) {
        return Err(UrlPolicyError::MissingHost);
    }
    validate_raw_path(raw_path(input))?;

    let mut url = Url::parse(input).map_err(|error| UrlPolicyError::InvalidUrl {
        reason: error.to_string(),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlPolicyError::UnsupportedScheme {
            scheme: url.scheme().to_owned(),
        });
    }
    if url.host_str().is_none() {
        return Err(UrlPolicyError::MissingHost);
    }
    if url.username() != "" || url.password().is_some() || has_userinfo(input) {
        return Err(UrlPolicyError::UserinfoNotAllowed);
    }

    validate_path(url.path())?;

    let raw_host = url.host_str().ok_or(UrlPolicyError::MissingHost)?;
    let host = canonical_host(raw_host)?;
    if !allow_private_network {
        if let Ok(address) = host.parse::<IpAddr>() {
            if let IpSafety::Special(reason) = classify_ip(address) {
                return Err(UrlPolicyError::UnsafeAddress { host, reason });
            }
        } else if let Some(reason) = special_hostname_reason(&host) {
            return Err(UrlPolicyError::UnsafeAddress { host, reason });
        }
    }

    if matches!(url.host(), Some(Host::Domain(_))) && raw_host != host {
        url.set_host(Some(&host))
            .map_err(|_| UrlPolicyError::NormalizationFailed {
                reason: "host normalization rejected the canonical host".to_owned(),
            })?;
    }

    let default_port = default_port(url.scheme()).expect("scheme checked above");
    if url.port() == Some(default_port) {
        url.set_port(None)
            .map_err(|_| UrlPolicyError::NormalizationFailed {
                reason: "default port removal failed".to_owned(),
            })?;
    }
    url.set_fragment(None);
    if matches!(query_mode, QueryMode::Drop) {
        url.set_query(None);
    }
    if url.path().is_empty() {
        url.set_path("/");
    }

    let origin = origin_from_url(&url)?;
    Ok(CanonicalUrl { url, origin })
}

fn origin_from_url(url: &Url) -> Result<Origin, UrlPolicyError> {
    let host = canonical_host(url.host_str().ok_or(UrlPolicyError::MissingHost)?)?;
    let port = url
        .port()
        .or_else(|| default_port(url.scheme()))
        .ok_or_else(|| UrlPolicyError::NormalizationFailed {
            reason: format!("no effective port for scheme `{}`", url.scheme()),
        })?;
    Ok(Origin {
        scheme: url.scheme().to_owned(),
        host,
        port,
    })
}

fn canonical_host(raw_host: &str) -> Result<String, UrlPolicyError> {
    let unbracketed = raw_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(raw_host);
    let host = unbracketed.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err(UrlPolicyError::InvalidHost);
    }
    Ok(host)
}

fn has_empty_authority(input: &str) -> bool {
    let Some(scheme_end) = input.find(':') else {
        return false;
    };
    input[scheme_end..].starts_with(":///")
}

fn has_userinfo(input: &str) -> bool {
    let Some(authority_start) = input.find("://").map(|index| index + 3) else {
        return false;
    };
    let remainder = &input[authority_start..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    remainder[..authority_end].contains('@')
}

fn raw_path(input: &str) -> &str {
    let authority_start = input.find("://").map_or(0, |index| index + 3);
    let remainder = &input[authority_start..];
    let path_start = remainder.find('/').unwrap_or(remainder.len());
    let path_and_suffix = &remainder[path_start..];
    let suffix_start = path_and_suffix
        .find(['?', '#'])
        .unwrap_or(path_and_suffix.len());
    &path_and_suffix[..suffix_start]
}

fn validate_raw_path(path: &str) -> Result<(), UrlPolicyError> {
    let decoded = percent_decode(path)?;
    for (raw_segment, decoded_segment) in path.split('/').zip(decoded.split(|byte| *byte == b'/')) {
        if (decoded_segment == b"." || decoded_segment == b"..") && raw_segment.contains('%') {
            return Err(UrlPolicyError::EncodedPathTraversal);
        }
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), UrlPolicyError> {
    let _ = percent_decode(path)?;
    Ok(())
}

fn percent_decode(path: &str) -> Result<Vec<u8>, UrlPolicyError> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(UrlPolicyError::InvalidUrl {
                    reason: "incomplete percent escape in path".to_owned(),
                });
            }
            let high = hex_value(bytes[index + 1]).ok_or_else(|| UrlPolicyError::InvalidUrl {
                reason: "invalid percent escape in path".to_owned(),
            })?;
            let low = hex_value(bytes[index + 2]).ok_or_else(|| UrlPolicyError::InvalidUrl {
                reason: "invalid percent escape in path".to_owned(),
            })?;
            let byte = high * 16 + low;
            if byte == b'/' || byte == b'\\' {
                return Err(UrlPolicyError::EncodedPathSeparator);
            }
            if byte == 0 || byte < 0x20 || byte == 0x7f {
                return Err(UrlPolicyError::EncodedPathTraversal);
            }
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(HTTP_DEFAULT_PORT),
        "https" => Some(HTTPS_DEFAULT_PORT),
        _ => None,
    }
}

fn is_ip_host(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok()
}

fn special_hostname_reason(host: &str) -> Option<&'static str> {
    if host == "localhost" || host.ends_with(".localhost") {
        return Some("localhost name");
    }
    if host == "local" || host.ends_with(".local") {
        return Some("mDNS local name");
    }
    None
}

fn classify_ipv4(address: Ipv4Addr) -> IpSafety {
    let value = u32::from(address);
    let ranges: &[(u32, u8, &str)] = &[
        (ipv4(0, 0, 0, 0), 8, "this-network"),
        (ipv4(10, 0, 0, 0), 8, "private"),
        (ipv4(172, 16, 0, 0), 12, "private"),
        (ipv4(100, 64, 0, 0), 10, "shared-address-space"),
        (ipv4(127, 0, 0, 0), 8, "loopback"),
        (ipv4(169, 254, 0, 0), 16, "link-local"),
        (ipv4(192, 0, 0, 0), 24, "special-purpose"),
        (ipv4(192, 0, 2, 0), 24, "documentation"),
        (ipv4(192, 31, 196, 0), 24, "as112"),
        (ipv4(192, 52, 193, 0), 24, "as112"),
        (ipv4(192, 88, 99, 0), 24, "deprecated-6to4-relay"),
        (ipv4(192, 168, 0, 0), 16, "private"),
        (ipv4(192, 175, 48, 0), 24, "as112"),
        (ipv4(198, 18, 0, 0), 15, "benchmarking"),
        (ipv4(198, 51, 100, 0), 24, "documentation"),
        (ipv4(203, 0, 113, 0), 24, "documentation"),
        (ipv4(224, 0, 0, 0), 4, "multicast"),
        (ipv4(240, 0, 0, 0), 4, "reserved"),
    ];
    for &(network, prefix, reason) in ranges {
        if ipv4_contains(value, network, prefix) {
            return IpSafety::Special(reason);
        }
    }
    IpSafety::Public
}

fn classify_ipv6(address: Ipv6Addr) -> IpSafety {
    let value = u128::from(address);
    let ranges: &[(u128, u8, &str)] = &[
        (ipv6(0, 0, 0, 0), 128, "unspecified"),
        (ipv6(0, 0, 0, 1), 128, "loopback"),
        (ipv6(0x0100, 0, 0, 0), 64, "discard-only"),
        (ipv6(0x2001, 0, 0, 0), 32, "teredo"),
        (ipv6(0x2001, 0x0001, 0, 0), 32, "special-purpose"),
        (ipv6(0x2001, 0x0002, 0, 0), 48, "benchmarking"),
        (ipv6(0x2001, 0x0004, 0x0112, 0), 48, "as112"),
        (ipv6(0x2001, 0x0010, 0, 0), 28, "orchid"),
        (ipv6(0x2001, 0x0020, 0, 0), 28, "orchid"),
        (ipv6(0x2001, 0x0db8, 0, 0), 32, "documentation"),
        (ipv6(0x2002, 0, 0, 0), 16, "6to4"),
        (ipv6(0x3ffe, 0, 0, 0), 16, "6bone"),
        (ipv6(0x3fff, 0, 0, 0), 20, "documentation"),
        (ipv6(0x0064, 0xff9b, 0, 0), 96, "nat64"),
        (ipv6(0xfc00, 0, 0, 0), 7, "unique-local"),
        (ipv6(0xfec0, 0, 0, 0), 10, "site-local"),
        (ipv6(0xfe80, 0, 0, 0), 10, "link-local"),
        (ipv6(0xff00, 0, 0, 0), 8, "multicast"),
    ];
    for &(network, prefix, reason) in ranges {
        if ipv6_contains(value, network, prefix) {
            return IpSafety::Special(reason);
        }
    }
    IpSafety::Public
}

const fn ipv4(a: u8, b: u8, c: u8, d: u8) -> u32 {
    u32::from_be_bytes([a, b, c, d])
}

const fn ipv6(a: u16, b: u16, c: u16, d: u16) -> u128 {
    u128::from_be_bytes([
        (a >> 8) as u8,
        a as u8,
        (b >> 8) as u8,
        b as u8,
        (c >> 8) as u8,
        c as u8,
        (d >> 8) as u8,
        d as u8,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ])
}

fn ipv4_contains(value: u32, network: u32, prefix: u8) -> bool {
    if prefix == 0 {
        true
    } else {
        let mask = u32::MAX << (32 - prefix);
        value & mask == network & mask
    }
}

fn ipv6_contains(value: u128, network: u128, prefix: u8) -> bool {
    if prefix == 0 {
        true
    } else {
        let mask = u128::MAX << (128 - prefix);
        value & mask == network & mask
    }
}

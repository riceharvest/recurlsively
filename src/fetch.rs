//! Bounded, redirect-aware HTTP fetching with private-network protection.

use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use crate::url_policy::{self, IpSafety};

const MAX_REDIRECT_HOPS: usize = 10;
const SNIFF_LIMIT: usize = 1024;

#[derive(Debug)]
pub enum FetchError {
    Network(String),
    Timeout,
    TooManyRedirects,
    RedirectCrossOrigin(String),
    RedirectDowngrade,
    BodyTooLarge {
        limit: u64,
    },
    NotHtml {
        status: u16,
        content_type: Option<String>,
    },
    UnsafeAddress {
        address: IpAddr,
        reason: String,
    },
    Status {
        status: u16,
        retryable: bool,
    },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::Timeout => write!(f, "request timed out"),
            Self::TooManyRedirects => write!(f, "more than {MAX_REDIRECT_HOPS} redirects"),
            Self::RedirectCrossOrigin(u) => write!(f, "cross-origin redirect to {u}"),
            Self::RedirectDowngrade => write!(f, "https to http redirect rejected"),
            Self::BodyTooLarge { limit } => write!(f, "body exceeds {limit} byte limit"),
            Self::NotHtml {
                status,
                content_type,
            } => {
                write!(
                    f,
                    "status {status} is not HTML (content-type {content_type:?})"
                )
            }
            Self::UnsafeAddress { address, reason } => {
                write!(f, "refusing unsafe address {address}: {reason}")
            }
            Self::Status { status, retryable } => {
                write!(f, "HTTP {status} (retryable: {retryable})")
            }
        }
    }
}

pub struct Fetched {
    pub final_url: String,
    pub status: u16,
    pub body: Vec<u8>,
}

pub struct Fetcher {
    client: reqwest::Client,
    allow_private_network: bool,
}

fn is_retryable(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || (500..600).contains(&status)
}

/// Resolves every address for `host` and rejects private/special ranges.
fn check_host_addresses(
    host: &str,
    port: u16,
    allow_private_network: bool,
) -> Result<(), FetchError> {
    if allow_private_network {
        return Ok(());
    }
    let mut checked = HashSet::new();
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| FetchError::Network(format!("DNS resolution failed for {host}: {e}")))?;
    for socket in addrs {
        if checked.insert(socket.ip()) {
            if let IpSafety::Special(reason) = url_policy::classify_ip(socket.ip()) {
                return Err(FetchError::UnsafeAddress {
                    address: socket.ip(),
                    reason: reason.to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn same_host(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
        && a.scheme() == b.scheme()
}

impl Fetcher {
    pub fn new(user_agent: &str, timeout: Duration, allow_private_network: bool) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client with valid configuration");
        Self {
            client,
            allow_private_network,
        }
    }

    /// Fetches `url` following at most [`MAX_REDIRECT_HOPS`] same-origin hops.
    pub async fn get(&self, url: &str, max_body: u64) -> Result<Fetched, FetchError> {
        let mut current: reqwest::Url = url
            .parse()
            .map_err(|e| FetchError::Network(format!("invalid url {url}: {e}")))?;
        let start_origin = current.clone();
        for _hop in 0..=MAX_REDIRECT_HOPS {
            check_host_addresses(
                current
                    .host_str()
                    .ok_or_else(|| FetchError::Network(format!("url {current} has no host")))?,
                current.port_or_known_default().unwrap_or(80),
                self.allow_private_network,
            )?;
            let response = self.client.get(current.clone()).send().await.map_err(|e| {
                if e.is_timeout() {
                    FetchError::Timeout
                } else {
                    FetchError::Network(e.to_string())
                }
            })?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let Some(location) = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                else {
                    return Err(FetchError::Network(format!(
                        "status {status} without Location header"
                    )));
                };
                let next = current
                    .join(location)
                    .map_err(|e| FetchError::Network(format!("bad redirect target: {e}")))?;
                if next.scheme() == "http" && current.scheme() == "https" {
                    return Err(FetchError::RedirectDowngrade);
                }
                if !same_host(&start_origin, &next) {
                    return Err(FetchError::RedirectCrossOrigin(next.to_string()));
                }
                current = next;
                continue;
            }
            if !(200..300).contains(&status) {
                return Err(FetchError::Status {
                    status,
                    retryable: is_retryable(status),
                });
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if !looks_like_html(&content_type) {
                return Err(FetchError::NotHtml {
                    status,
                    content_type,
                });
            }
            let content_length = response.content_length();
            if let Some(length) = content_length {
                if length > max_body {
                    return Err(FetchError::BodyTooLarge { limit: max_body });
                }
            }
            let body = read_bounded(response, max_body).await?;
            return Ok(Fetched {
                final_url: current.to_string(),
                status,
                body,
            });
        }
        Err(FetchError::TooManyRedirects)
    }
}

fn looks_like_html(content_type: &Option<String>) -> bool {
    let Some(value) = content_type else {
        return true; // bounded sniff happens after reading a small prefix
    };
    let lower = value.to_ascii_lowercase();
    lower.contains("text/html") || lower.contains("application/xhtml+xml")
}

async fn read_bounded(response: reqwest::Response, max_body: u64) -> Result<Vec<u8>, FetchError> {
    let mut body = Vec::new();
    let mut chunk_stream = response;
    while let Some(chunk) = chunk_stream.chunk().await.map_err(|e| {
        if e.is_timeout() {
            FetchError::Timeout
        } else {
            FetchError::Network(e.to_string())
        }
    })? {
        if body.len() as u64 + chunk.len() as u64 > max_body {
            return Err(FetchError::BodyTooLarge { limit: max_body });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Bounded sniff for responses that arrived without a Content-Type.
pub fn sniff_html_prefix(body: &[u8]) -> bool {
    let prefix = &body[..body.len().min(SNIFF_LIMIT)];
    let lower = prefix.to_ascii_lowercase();
    lower.starts_with(b"<!doctype html") || lower.windows(5).any(|w| w == b"<html")
}

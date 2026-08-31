//! Safe DNS resolution: every address returned to the HTTP client is
//! checked against the private/special policy, closing the DNS-rebinding
//! gap between an upfront check and the actual TCP connect.

use std::collections::HashSet;
use std::net::ToSocketAddrs;

use crate::url_policy::{self, IpSafety};

pub use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// A reqwest `Resolve` implementation that refuses to hand the client any
/// address classified as private or special unless explicitly allowed.
pub struct SafeResolver {
    allow_private_network: bool,
}

impl SafeResolver {
    pub fn new(allow_private_network: bool) -> Self {
        Self {
            allow_private_network,
        }
    }
}

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow = self.allow_private_network;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<_> = (host.as_str(), 0u16)
                .to_socket_addrs()
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no addresses resolved for {host}"),
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            let mut checked = HashSet::new();
            let mut safe: Vec<std::net::SocketAddr> = Vec::with_capacity(addrs.len());
            for socket in addrs {
                if checked.insert(socket.ip()) && !allow {
                    if let IpSafety::Special(reason) = url_policy::classify_ip(socket.ip()) {
                        return Err(Box::new(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            format!("refusing unsafe address {}: {reason}", socket.ip()),
                        ))
                            as Box<dyn std::error::Error + Send + Sync>);
                    }
                }
                safe.push(socket);
            }
            Ok(Box::new(safe.into_iter()) as Addrs)
        })
    }
}

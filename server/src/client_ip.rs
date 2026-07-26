//! Trusted client-IP selection shared by rate limiting and session hashing.

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use std::net::{IpAddr, SocketAddr};
use tower_governor::GovernorError;
use tower_governor::key_extractor::KeyExtractor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIpKeyExtractor {
    trust_proxy_headers: bool,
}

impl ClientIpKeyExtractor {
    pub fn new(trust_proxy_headers: bool) -> Self {
        Self {
            trust_proxy_headers,
        }
    }
}

impl KeyExtractor for ClientIpKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        select_client_ip(
            req.headers(),
            peer_ip_from_extensions(req),
            self.trust_proxy_headers,
        )
        .ok_or(GovernorError::UnableToExtractKey)
    }
}

pub fn select_client_ip(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
    trust_proxy_headers: bool,
) -> Option<IpAddr> {
    if trust_proxy_headers {
        x_forwarded_for(headers)
            .or_else(|| x_real_ip(headers))
            .or(peer_ip)
    } else {
        peer_ip
    }
}

pub fn peer_ip_from_extensions<T>(req: &Request<T>) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|addr| addr.0.ip())
        .or_else(|| req.extensions().get::<SocketAddr>().map(|addr| addr.ip()))
}

fn x_forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .and_then(|s| s.parse().ok())
}

fn x_real_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn ignores_forwarded_headers_by_default() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));
        let peer = "10.0.0.1".parse().ok();

        assert_eq!(select_client_ip(&headers, peer, false), peer);
    }

    #[test]
    fn trusted_mode_prefers_x_forwarded_for_then_x_real_ip_then_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.10, 10.0.0.1"),
        );
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.20"));
        let peer = "10.0.0.1".parse().ok();

        assert_eq!(
            select_client_ip(&headers, peer, true),
            "203.0.113.10".parse().ok()
        );

        headers.remove("x-forwarded-for");
        assert_eq!(
            select_client_ip(&headers, peer, true),
            "198.51.100.20".parse().ok()
        );

        headers.remove("x-real-ip");
        assert_eq!(select_client_ip(&headers, peer, true), peer);
    }
}

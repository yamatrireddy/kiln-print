//! Pre-authentication request checks: peer address, `Host` (DNS rebinding) and `Origin`.

use std::net::SocketAddr;

use axum::http::{HeaderMap, StatusCode, header};

use super::ConnectionInfo;

/// Normalises an origin to `scheme://host[:port]` (lowercase, default port removed).
/// Returns `None` for anything that is not a plain http(s) origin, including the
/// opaque `null` origin sent by sandboxed frames and `file://` pages.
pub fn normalise_origin(origin: &str) -> Option<String> {
    let origin = origin.trim().trim_end_matches('/');
    let (scheme, rest) = origin.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => "80",
        "https" => "443",
        _ => return None,
    };
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) {
        return None;
    }
    let rest = rest.to_ascii_lowercase();
    let (host, port) = split_host_port(&rest)?;
    if host.is_empty() {
        return None;
    }
    Some(match port {
        Some(p) if p != default_port => format!("{scheme}://{host}:{p}"),
        _ => format!("{scheme}://{host}"),
    })
}

/// Splits `host[:port]`, handling bracketed IPv6 literals. Validates the port.
fn split_host_port(value: &str) -> Option<(&str, Option<&str>)> {
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &value[..end + 2];
        match &rest[end + 1..] {
            "" => (host, None),
            p => (host, Some(p.strip_prefix(':')?)),
        }
    } else {
        match value.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (value, None),
        }
    };
    if let Some(p) = port {
        p.parse::<u16>().ok()?;
    }
    Some((host, port))
}

#[derive(Debug, Clone)]
pub struct RequestGuard {
    pub allow_non_loopback: bool,
}

impl RequestGuard {
    /// Validates a request before authentication. Returns the connection facts or the
    /// HTTP status to reject with.
    pub fn check(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
    ) -> Result<ConnectionInfo, (StatusCode, &'static str)> {
        if !self.allow_non_loopback {
            if !peer.ip().is_loopback() {
                return Err((StatusCode::FORBIDDEN, "remote connections are disabled"));
            }
            // DNS rebinding: a hostile page can resolve its own domain to 127.0.0.1, but
            // the browser still sends that domain in `Host`.
            let host = headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            if !is_loopback_host(host) {
                return Err((StatusCode::FORBIDDEN, "invalid Host header"));
            }
        }
        let origin = match headers.get(header::ORIGIN) {
            None => None,
            Some(value) => {
                let raw = value
                    .to_str()
                    .map_err(|_| (StatusCode::FORBIDDEN, "invalid Origin header"))?;
                Some(normalise_origin(raw).ok_or((StatusCode::FORBIDDEN, "origin not allowed"))?)
            }
        };
        let user_agent = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().take(200).collect());
        Ok(ConnectionInfo {
            peer,
            origin,
            user_agent,
        })
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let Some((name, _)) = split_host_port(&host) else {
        return false;
    };
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn origin_normalisation() {
        assert_eq!(
            normalise_origin("https://Lab.Example.com:443/").as_deref(),
            Some("https://lab.example.com")
        );
        assert_eq!(
            normalise_origin("http://localhost:3000").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            normalise_origin("http://[::1]:8080").as_deref(),
            Some("http://[::1]:8080")
        );
        for bad in [
            "null",
            "file://",
            "chrome-extension://abc",
            "https://a.com/path",
            "https://a.com:99999",
            "https://",
        ] {
            assert_eq!(normalise_origin(bad), None, "{bad}");
        }
    }

    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).expect("header"));
        if let Some(o) = origin {
            h.insert(header::ORIGIN, HeaderValue::from_str(o).expect("header"));
        }
        h
    }

    #[test]
    fn host_header_must_be_loopback() {
        let guard = RequestGuard {
            allow_non_loopback: false,
        };
        let peer: SocketAddr = "127.0.0.1:5000".parse().expect("addr");
        assert!(guard.check(peer, &headers("localhost:18731", None)).is_ok());
        assert!(guard.check(peer, &headers("127.0.0.1:18731", None)).is_ok());
        assert!(guard.check(peer, &headers("[::1]:18731", None)).is_ok());
        assert!(
            guard
                .check(peer, &headers("evil.example:18731", None))
                .is_err()
        );
        assert!(
            guard
                .check(peer, &headers("localhost.evil.example", None))
                .is_err()
        );
    }

    #[test]
    fn remote_peers_are_refused() {
        let guard = RequestGuard {
            allow_non_loopback: false,
        };
        let peer: SocketAddr = "192.168.1.9:5000".parse().expect("addr");
        assert!(guard.check(peer, &headers("localhost", None)).is_err());
    }

    #[test]
    fn opaque_origins_are_refused() {
        let guard = RequestGuard {
            allow_non_loopback: false,
        };
        let peer: SocketAddr = "127.0.0.1:5000".parse().expect("addr");
        assert!(
            guard
                .check(peer, &headers("localhost", Some("null")))
                .is_err()
        );
        let info = guard
            .check(peer, &headers("localhost", Some("HTTPS://App.Example")))
            .expect("ok");
        assert_eq!(info.origin.as_deref(), Some("https://app.example"));
    }
}

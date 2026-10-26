//! The client IP for rate limiting, read from the TCP peer or from a header
//! that a trusted reverse proxy sets.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName};
use color_eyre::eyre::eyre;
use ipnet::IpNet;

use super::AppState;
use super::error::ApiError;

/// Where the client IP comes from: the `header` when the TCP peer is one of
/// the `trusted_proxies`, the peer otherwise.
///
/// Behind a reverse proxy every peer is the proxy, so without the header all
/// clients would share one rate-limit bucket. Peers outside
/// `trusted_proxies` can't set the client IP, because any client can send
/// any header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIpSource {
    header: Option<HeaderName>,
    trusted_proxies: Vec<IpNet>,
}

/// The `trusted_proxies` default: a reverse proxy on the same host.
pub const DEFAULT_TRUSTED_PROXIES: &[&str] = &["127.0.0.0/8", "::1/128"];

/// A `client_ip_header` that isn't a valid header name, or a
/// `trusted_proxies` entry that is neither an IP address nor a CIDR.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidClientIpSource {
    #[error("client_ip_header {0:?} is not a valid header name")]
    Header(String),
    #[error("trusted_proxies entry {0:?} is not an IP address or CIDR")]
    Proxy(String),
}

impl ClientIpSource {
    pub fn parse<S: AsRef<str>>(
        header: Option<&str>,
        trusted_proxies: &[S],
    ) -> Result<ClientIpSource, InvalidClientIpSource> {
        let header = header
            .map(|name| {
                HeaderName::try_from(name.trim())
                    .map_err(|_| InvalidClientIpSource::Header(name.to_owned()))
            })
            .transpose()?;
        let trusted_proxies = trusted_proxies
            .iter()
            .map(|entry| {
                let entry = entry.as_ref().trim();
                entry
                    .parse::<IpNet>()
                    .map(|network| network.trunc())
                    .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                    .map_err(|_| InvalidClientIpSource::Proxy(entry.to_owned()))
            })
            .collect::<Result<_, _>>()?;
        Ok(ClientIpSource {
            header,
            trusted_proxies,
        })
    }

    /// The client IP of a request from `peer`.
    pub fn resolve(&self, peer: IpAddr, headers: &HeaderMap) -> Resolved {
        // A proxy on a dual-stack socket shows up as `::ffff:127.0.0.1`,
        // which the IPv4 entries must still match.
        let peer = peer.to_canonical();
        let Some(header) = &self.header else {
            return Resolved::Peer(peer);
        };
        if !self
            .trusted_proxies
            .iter()
            .any(|proxy| proxy.contains(&peer))
        {
            return Resolved::Peer(peer);
        }
        // The header must carry exactly one address. A list
        // (`X-Forwarded-For: client, hop`) or a repeated header means the
        // proxy appends to what the client sent, so no value is safe to use.
        let mut values = headers.get_all(header).iter();
        let client = match (values.next(), values.next()) {
            (Some(value), None) => value
                .to_str()
                .ok()
                .and_then(|value| value.trim().parse::<IpAddr>().ok()),
            _ => None,
        };
        match client {
            Some(client) => Resolved::Header(client.to_canonical()),
            None => Resolved::Fallback(peer),
        }
    }
}

/// The outcome of [`ClientIpSource::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// No header is configured, or the peer isn't a trusted proxy.
    Peer(IpAddr),
    /// The trusted proxy's header named the client.
    Header(IpAddr),
    /// A trusted proxy sent no usable header, so the peer stands in. Every
    /// client behind that proxy then shares one bucket, which points at a
    /// proxy misconfiguration worth logging.
    Fallback(IpAddr),
}

impl Resolved {
    pub fn ip(self) -> IpAddr {
        match self {
            Resolved::Peer(ip) | Resolved::Header(ip) | Resolved::Fallback(ip) => ip,
        }
    }
}

/// Set after the first fallback warning, so a misconfigured proxy logs one
/// warning and then debug lines instead of one warning per request.
static WARNED_FALLBACK: AtomicBool = AtomicBool::new(false);

/// The client's IP address, for rate limiting.
///
/// The peer comes from `ConnectInfo`, which requires serving with
/// `into_make_service_with_connect_info::<SocketAddr>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // The extractor, not a raw extension lookup, so tests can supply
        // the peer with `MockConnectInfo`.
        let ConnectInfo(peer) = ConnectInfo::<SocketAddr>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| eyre!("read the peer address: {rejection}"))?;
        let resolved = state.client_ip.resolve(peer.ip(), &parts.headers);
        if let Resolved::Fallback(proxy) = resolved {
            if WARNED_FALLBACK.swap(true, Ordering::Relaxed) {
                tracing::debug!(%proxy, "trusted proxy sent no usable client IP header");
            } else {
                tracing::warn!(
                    %proxy,
                    "trusted proxy sent no usable client IP header, rate limiting \
                     by the proxy address; later occurrences log at debug"
                );
            }
        }
        Ok(Self(resolved.ip()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::try_from(*name).expect("valid header name"),
                value.parse().expect("valid header value"),
            );
        }
        headers
    }

    fn ip(literal: &str) -> IpAddr {
        literal.parse().expect("valid IP literal")
    }

    fn behind_loopback() -> ClientIpSource {
        ClientIpSource::parse(Some("X-Real-IP"), DEFAULT_TRUSTED_PROXIES)
            .expect("valid header and defaults")
    }

    #[test]
    fn trusted_proxy_header_names_the_client() {
        let source = behind_loopback();
        let real_ip = headers(&[("x-real-ip", " 2001:db8::7 ")]);
        assert_eq!(
            source.resolve(ip("127.0.0.1"), &real_ip),
            Resolved::Header(ip("2001:db8::7"))
        );
        assert_eq!(
            source.resolve(ip("::1"), &real_ip),
            Resolved::Header(ip("2001:db8::7"))
        );
        // nginx on a dual-stack socket.
        assert_eq!(
            source.resolve(ip("::ffff:127.0.0.1"), &real_ip),
            Resolved::Header(ip("2001:db8::7"))
        );
    }

    #[test]
    fn untrusted_peer_or_no_header_config_uses_the_peer() {
        let real_ip = headers(&[("x-real-ip", "198.51.100.1")]);
        assert_eq!(
            behind_loopback().resolve(ip("100.64.0.5"), &real_ip),
            Resolved::Peer(ip("100.64.0.5"))
        );
        let no_header =
            ClientIpSource::parse(None, DEFAULT_TRUSTED_PROXIES).expect("valid defaults");
        assert_eq!(
            no_header.resolve(ip("127.0.0.1"), &real_ip),
            Resolved::Peer(ip("127.0.0.1"))
        );
    }

    #[test]
    fn unusable_header_from_a_trusted_proxy_falls_back() {
        let source = behind_loopback();
        for unusable in [
            headers(&[]),
            headers(&[("x-real-ip", "")]),
            headers(&[("x-real-ip", "not-an-ip")]),
            headers(&[("x-real-ip", "198.51.100.1, 127.0.0.1")]),
            headers(&[("x-real-ip", "198.51.100.1"), ("x-real-ip", "198.51.100.2")]),
        ] {
            assert_eq!(
                source.resolve(ip("127.0.0.1"), &unusable),
                Resolved::Fallback(ip("127.0.0.1")),
                "{unusable:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_bad_entries() {
        assert_eq!(
            ClientIpSource::parse(Some("X Real IP"), DEFAULT_TRUSTED_PROXIES),
            Err(InvalidClientIpSource::Header("X Real IP".into()))
        );
        assert_eq!(
            ClientIpSource::parse(None, &["127.0.0.1", "localhost"]),
            Err(InvalidClientIpSource::Proxy("localhost".into()))
        );
    }
}

//! The rules of the outbound fetch guard (spec 3.2): which URLs and
//! addresses a server-side fetch may reach, and the DNS resolver that
//! enforces them at connect time.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use ipnet::IpNet;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

use super::{FetchError, FetchPolicy};

// ==============================================================================
// Address classification
// ==============================================================================

/// Why an address is off limits for a server-side fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    Loopback,
    /// RFC 1918 (10/8, 172.16/12, 192.168/16).
    Private,
    /// 169.254/16, which holds cloud metadata services, and fe80::/10.
    LinkLocal,
    /// Carrier-grade NAT, 100.64/10.
    Cgnat,
    /// fc00::/7 and the deprecated site-local fec0::/10.
    UniqueLocal,
    Multicast,
    /// 0.0.0.0/8 and ::.
    Unspecified,
    Broadcast,
}

impl fmt::Display for BlockReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BlockReason::Loopback => "loopback",
            BlockReason::Private => "private",
            BlockReason::LinkLocal => "link-local",
            BlockReason::Cgnat => "carrier-grade NAT",
            BlockReason::UniqueLocal => "unique-local",
            BlockReason::Multicast => "multicast",
            BlockReason::Unspecified => "unspecified",
            BlockReason::Broadcast => "broadcast",
        })
    }
}

/// A fetch target resolved to an address the guard refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("refusing to connect to {reason} address {addr}")]
pub struct BlockedAddress {
    pub addr: IpAddr,
    pub reason: BlockReason,
}

/// Returns why `addr` must not be fetched from, or `None` for a public
/// address.
pub fn classify(addr: IpAddr) -> Option<BlockReason> {
    match addr {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(addr: Ipv4Addr) -> Option<BlockReason> {
    let [a, b, ..] = addr.octets();
    let reason = if addr.is_loopback() {
        BlockReason::Loopback
    } else if addr.is_private() {
        BlockReason::Private
    } else if addr.is_link_local() {
        BlockReason::LinkLocal
    } else if a == 100 && (b & 0b1100_0000) == 64 {
        BlockReason::Cgnat
    } else if addr.is_multicast() {
        BlockReason::Multicast
    } else if a == 0 {
        // All of 0.0.0.0/8 ("this network"), not just 0.0.0.0: Linux routes
        // a connect to 0.0.0.0 to the local host.
        BlockReason::Unspecified
    } else if addr.is_broadcast() {
        BlockReason::Broadcast
    } else {
        return None;
    };
    Some(reason)
}

fn classify_v6(addr: Ipv6Addr) -> Option<BlockReason> {
    if addr.is_loopback() {
        return Some(BlockReason::Loopback);
    }
    if addr.is_unspecified() {
        return Some(BlockReason::Unspecified);
    }
    // An IPv4 address embedded in an IPv6 one reaches the IPv4 host, so it
    // gets the IPv4 verdict: IPv4-mapped (::ffff:a.b.c.d), the deprecated
    // IPv4-compatible form (::a.b.c.d) and the NAT64 well-known prefix
    // (64:ff9b::/96), which a NAT64 gateway translates to the IPv4 address.
    let segments = addr.segments();
    let embeds_v4 = matches!(
        segments,
        [0, 0, 0, 0, 0, 0xffff, _, _] | [0, 0, 0, 0, 0, 0, _, _]
    ) || matches!(segments, [0x64, 0xff9b, 0, 0, 0, 0, _, _]);
    if embeds_v4 {
        let [.., a, b, c, d] = addr.octets();
        return classify_v4(Ipv4Addr::new(a, b, c, d));
    }
    // TODO: 6to4 (2002::/16) and Teredo (2001::/32) also embed IPv4
    // addresses, but reaching them needs a relay, which pod's hosts lack.
    let first = segments[0];
    let reason = if first & 0xffc0 == 0xfe80 {
        BlockReason::LinkLocal
    } else if first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfec0 {
        // fec0::/10 is the deprecated site-local range, the predecessor of
        // unique-local addresses, and equally internal.
        BlockReason::UniqueLocal
    } else if addr.is_multicast() {
        BlockReason::Multicast
    } else {
        return None;
    };
    Some(reason)
}

// ==============================================================================
// Allowlist
// ==============================================================================

/// Hosts and networks that bypass the address rules, for feeds self-hosted on
/// a LAN. Built from the `fetch_allowlist` config key.
///
/// An entry that parses as an IP address or CIDR (`192.168.1.20`,
/// `10.0.0.0/8`, `fd00::/8`) allows every address inside it, whatever host
/// name led there. Any other entry is a host name (`nas.lan`) whose resolved
/// addresses are all allowed. Host names match exactly, without wildcards.
///
/// A URL whose host the allowlist names may also use any port, since LAN
/// servers often listen on 8080 and the like (see [`check_url`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowlist {
    hosts: Vec<String>,
    networks: Vec<IpNet>,
}

/// A `fetch_allowlist` entry that is neither an address, a CIDR nor a valid
/// host name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("fetch allowlist entry {0:?} is not a host name, IP address or CIDR")]
pub struct InvalidAllowlistEntry(pub String);

impl Allowlist {
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Allowlist, InvalidAllowlistEntry> {
        let mut allowlist = Allowlist::default();
        for entry in entries {
            let entry = entry.as_ref().trim();
            let invalid = || InvalidAllowlistEntry(entry.to_owned());
            if let Ok(network) = entry.parse::<IpNet>() {
                allowlist.networks.push(network.trunc());
            } else if let Ok(addr) = entry.parse::<IpAddr>() {
                allowlist.networks.push(IpNet::from(addr));
            } else {
                // `Host::parse` normalizes the name the same way URL parsing
                // does (lowercase, punycode), so entries compare equal to
                // the hosts of parsed URLs.
                match Host::parse(entry).map_err(|_| invalid())? {
                    Host::Domain(domain) => allowlist.hosts.push(canonical_host(&domain)),
                    // Bracketed IPv6 and IPv4 forms the `IpAddr` parser
                    // rejects (`127.1`) are still single addresses.
                    Host::Ipv4(addr) => allowlist.networks.push(IpNet::from(IpAddr::V4(addr))),
                    Host::Ipv6(addr) => allowlist.networks.push(IpNet::from(IpAddr::V6(addr))),
                }
            }
        }
        Ok(allowlist)
    }

    fn allows_host(&self, host: &str) -> bool {
        let host = canonical_host(host);
        self.hosts.contains(&host)
    }

    fn allows_addr(&self, addr: IpAddr) -> bool {
        self.networks.iter().any(|network| network.contains(&addr))
    }

    /// Whether a URL host is allowlisted itself: a host name by a host-name
    /// entry, a literal IP by an address or CIDR entry. A host name that only
    /// resolves into an allowlisted network doesn't count.
    fn names(&self, host: &Host<&str>) -> bool {
        match *host {
            Host::Domain(domain) => self.allows_host(domain),
            Host::Ipv4(addr) => self.allows_addr(IpAddr::V4(addr)),
            Host::Ipv6(addr) => self.allows_addr(IpAddr::V6(addr)),
        }
    }

    /// The verdict on connecting to `addr`, reached through a host that
    /// `host_allowed` says is allowlisted by name.
    fn check(&self, host_allowed: bool, addr: IpAddr) -> Result<(), BlockedAddress> {
        if host_allowed || self.allows_addr(addr) {
            return Ok(());
        }
        match classify(addr) {
            None => Ok(()),
            Some(reason) => Err(BlockedAddress { addr, reason }),
        }
    }
}

/// `nas.lan.` and `nas.lan` name the same host.
fn canonical_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

// ==============================================================================
// URL checks
// ==============================================================================

/// Checks everything about `url` that can be checked before connecting:
/// scheme, port, and the address of a literal IP host. Host names are checked
/// later, by [`GuardedResolver`], because only the resolver sees the
/// addresses the connection will use.
///
/// A host the allowlist names may use any port; every other host is limited
/// to [`FetchPolicy::allowed_ports`]. The port is judged here, before DNS,
/// and the resolver never learns it, so a host name that merely resolves
/// into an allowlisted network keeps the port rule. Opening ports by resolved
/// address would need the resolver to know the port, and would let anyone
/// who controls a public name's DNS point it into the allowlisted network to
/// reach every service there. Naming the host (`nas.lan`) or using its
/// literal address is the operator's opt-in.
///
/// A subscribe runs it too, so a URL the poller could never fetch is
/// rejected up front instead of becoming a podcast that only ever fails.
pub fn check_url(url: &Url, policy: &FetchPolicy) -> Result<(), FetchError> {
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(FetchError::UnsupportedScheme(other.to_owned())),
    }
    let host = url.host().ok_or(FetchError::MissingHost)?;
    let port = url
        .port_or_known_default()
        .expect("http and https have default ports");
    if !policy.allowed_ports.contains(&port) && !policy.allowlist.names(&host) {
        return Err(FetchError::PortNotAllowed(port));
    }
    // reqwest connects to literal IP hosts without asking the resolver, so
    // they are checked here.
    let addr = match host {
        Host::Domain(_) => return Ok(()),
        Host::Ipv4(addr) => IpAddr::V4(addr),
        Host::Ipv6(addr) => IpAddr::V6(addr),
    };
    policy.allowlist.check(false, addr)?;
    Ok(())
}

// ==============================================================================
// Resolver
// ==============================================================================

/// Turns a host name into addresses. The production implementation is
/// [`SystemLookup`]; tests script answers to simulate DNS rebinding.
pub trait Lookup: Send + Sync + 'static {
    fn lookup(&self, host: &str) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>>;
}

/// The operating system's resolver (`getaddrinfo`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup(&self, host: &str) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>> {
        let host = host.to_owned();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok(addrs.map(|addr| addr.ip()).collect())
        })
    }
}

/// A host name that didn't resolve to any address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("could not resolve the host: {0}")]
pub struct ResolveFailed(pub String);

/// The reqwest DNS resolver that enforces the address rules.
///
/// reqwest connects only to the addresses a resolver returns, so checking
/// them here means the checked addresses are exactly the ones connected to.
/// A rebinding DNS server can't answer the check with a public address and
/// the connection with an internal one, because there is no second lookup.
/// Every address must pass, not just one: otherwise a host with one public
/// and one internal record would leave the choice to connection order.
pub(super) struct GuardedResolver {
    pub(super) lookup: Arc<dyn Lookup>,
    pub(super) allowlist: Arc<Allowlist>,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host_allowed = self.allowlist.allows_host(name.as_str());
        let lookup = self.lookup.lookup(name.as_str());
        let allowlist = Arc::clone(&self.allowlist);
        Box::pin(async move {
            let ips = lookup
                .await
                .map_err(|error| ResolveFailed(error.to_string()))?;
            if ips.is_empty() {
                return Err(ResolveFailed("no addresses".to_owned()).into());
            }
            for &ip in &ips {
                allowlist.check(host_allowed, ip)?;
            }
            // The port is a placeholder; the connector replaces it with the
            // URL's port.
            let addrs: Addrs = Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    fn policy(allowlist: &[&str]) -> FetchPolicy {
        FetchPolicy::new(Allowlist::parse(allowlist).expect("test allowlists are valid"))
    }

    fn url(input: &str) -> Url {
        Url::parse(input).expect("test URLs are valid")
    }

    /// Answers each lookup with the next scripted answer, like a rebinding
    /// DNS server with a zero TTL.
    struct ScriptedLookup(Mutex<VecDeque<Vec<IpAddr>>>);

    impl ScriptedLookup {
        fn new(answers: &[&[&str]]) -> ScriptedLookup {
            let answers = answers
                .iter()
                .map(|answer| {
                    answer
                        .iter()
                        .map(|ip| ip.parse().expect("test literals are valid addresses"))
                        .collect()
                })
                .collect();
            ScriptedLookup(Mutex::new(answers))
        }
    }

    impl Lookup for ScriptedLookup {
        fn lookup(
            &self,
            _host: &str,
        ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>> {
            let answer = self
                .0
                .lock()
                .expect("no test panics while holding the lock")
                .pop_front()
                .expect("the test scripts enough answers");
            Box::pin(async move { Ok(answer) })
        }
    }

    fn resolver(lookup: ScriptedLookup, allowlist: &[&str]) -> GuardedResolver {
        GuardedResolver {
            lookup: Arc::new(lookup),
            allowlist: Arc::new(Allowlist::parse(allowlist).expect("test allowlists are valid")),
        }
    }

    async fn resolve(resolver: &GuardedResolver, host: &str) -> Result<Vec<IpAddr>, String> {
        let name: Name = host.parse().expect("test host names are valid");
        match resolver.resolve(name).await {
            Ok(addrs) => Ok(addrs.map(|addr| addr.ip()).collect()),
            Err(error) => Err(error.to_string()),
        }
    }

    #[tokio::test]
    async fn rebinding_answer_is_checked_on_every_resolution() {
        let resolver = resolver(
            ScriptedLookup::new(&[&["93.184.215.14"], &["127.0.0.1"]]),
            &[],
        );
        assert_eq!(
            resolve(&resolver, "rebind.example").await,
            Ok(vec!["93.184.215.14".parse().expect("valid literal")])
        );
        let second = resolve(&resolver, "rebind.example").await;
        assert_eq!(
            second,
            Err("refusing to connect to loopback address 127.0.0.1".to_owned())
        );
    }

    #[tokio::test]
    async fn one_internal_address_blocks_the_whole_answer() {
        let resolver = resolver(
            ScriptedLookup::new(&[&["93.184.215.14", "169.254.169.254"]]),
            &[],
        );
        let result = resolve(&resolver, "mixed.example").await;
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("link-local")),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn allowlisted_host_name_may_resolve_internally() {
        let resolver = resolver(ScriptedLookup::new(&[&["192.168.1.20"]]), &["NAS.lan."]);
        assert!(resolve(&resolver, "nas.lan").await.is_ok());
    }

    #[tokio::test]
    async fn allowlisted_network_admits_any_host_inside_it() {
        let resolver = resolver(
            ScriptedLookup::new(&[&["10.1.2.3"], &["10.1.2.3", "192.168.0.1"]]),
            &["10.0.0.0/8"],
        );
        assert!(resolve(&resolver, "a.lan").await.is_ok());
        let result = resolve(&resolver, "b.lan").await;
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("192.168.0.1")),
            "{result:?}"
        );
    }

    #[test]
    fn allowlist_entries() {
        let allowlist = Allowlist::parse(&[
            " nas.lan ",
            "192.168.1.20",
            "10.9.0.0/16",
            "fd00::/8",
            "[fe80::1]",
        ])
        .expect("all entries are valid");
        assert!(allowlist.allows_host("nas.lan"));
        assert!(!allowlist.allows_host("other.lan"));
        for allowed in ["192.168.1.20", "10.9.200.1", "fd12::1", "fe80::1"] {
            let addr = allowed.parse().expect("valid literal");
            assert!(allowlist.allows_addr(addr), "{allowed}");
        }
        for blocked in ["192.168.1.21", "10.10.0.1", "fe80::2"] {
            let addr = blocked.parse().expect("valid literal");
            assert!(!allowlist.allows_addr(addr), "{blocked}");
        }

        for invalid in ["10.0.0.0/33", "nas lan", "http://nas.lan", "a/b"] {
            assert_eq!(
                Allowlist::parse(&[invalid]),
                Err(InvalidAllowlistEntry(invalid.to_owned())),
                "{invalid}"
            );
        }
    }

    #[test]
    fn url_scheme_and_port_rules() {
        let policy = policy(&[]);
        assert!(check_url(&url("https://feeds.example/rss"), &policy).is_ok());
        assert!(check_url(&url("http://feeds.example:443/rss"), &policy).is_ok());
        assert!(matches!(
            check_url(&url("ftp://feeds.example/rss"), &policy),
            Err(FetchError::UnsupportedScheme(scheme)) if scheme == "ftp"
        ));
        assert!(matches!(
            check_url(&url("file:///etc/passwd"), &policy),
            Err(FetchError::UnsupportedScheme(_))
        ));
        for port in [22, 8080, 6379] {
            let target = url(&format!("http://feeds.example:{port}/rss"));
            assert!(
                matches!(check_url(&target, &policy), Err(FetchError::PortNotAllowed(p)) if p == port),
                "{port}"
            );
        }
    }

    #[test]
    fn allowlisted_hosts_may_use_any_port() {
        let policy = policy(&["NAS.lan.", "192.168.1.0/24", "fd00::/8"]);
        for allowed in [
            "http://nas.lan:8080/rss",
            "http://192.168.1.20:8080/rss",
            "https://[fd12::1]:8443/rss",
        ] {
            assert!(check_url(&url(allowed), &policy).is_ok(), "{allowed}");
        }
        // A name is judged by its entry, not by where it resolves: the port
        // is checked before DNS, so `media.lan` needs 80 or 443 even if it
        // resolves into 192.168.1.0/24.
        for refused in [
            "http://media.lan:8080/rss",
            "http://feeds.example:8080/rss",
            "http://192.168.2.20:8080/rss",
        ] {
            assert!(
                matches!(
                    check_url(&url(refused), &policy),
                    Err(FetchError::PortNotAllowed(8080))
                ),
                "{refused}"
            );
        }
    }

    #[test]
    fn literal_ip_hosts() {
        let open = policy(&[]);
        for (literal, reason) in [
            ("http://127.0.0.1/", BlockReason::Loopback),
            // WHATWG URL parsing turns these shorthand forms into 127.0.0.1.
            ("http://127.1/", BlockReason::Loopback),
            ("http://2130706433/", BlockReason::Loopback),
            ("http://0x7f.0.0.1/", BlockReason::Loopback),
            (
                "http://169.254.169.254/latest/meta-data/",
                BlockReason::LinkLocal,
            ),
            ("http://[::1]/", BlockReason::Loopback),
            ("http://[::ffff:127.0.0.1]/", BlockReason::Loopback),
            ("http://[fe80::1]/", BlockReason::LinkLocal),
            ("http://0.0.0.0/", BlockReason::Unspecified),
        ] {
            let result = check_url(&url(literal), &open);
            assert!(
                matches!(result, Err(FetchError::Blocked(BlockedAddress { reason: r, .. })) if r == reason),
                "{literal}: {result:?}"
            );
        }
        assert!(check_url(&url("http://93.184.215.14/"), &open).is_ok());
        assert!(check_url(&url("http://192.168.1.20/"), &policy(&["192.168.1.20"])).is_ok());
        assert!(check_url(&url("http://[::1]/"), &policy(&["::1"])).is_ok());
    }

    #[test]
    fn classification_table() {
        use BlockReason::*;
        let cases: &[(&str, Option<BlockReason>)] = &[
            ("93.184.215.14", None),
            ("1.1.1.1", None),
            ("100.63.255.255", None),
            ("100.128.0.0", None),
            ("172.32.0.1", None),
            ("2606:4700:4700::1111", None),
            ("127.0.0.1", Some(Loopback)),
            ("127.255.255.254", Some(Loopback)),
            ("10.0.0.1", Some(Private)),
            ("172.16.0.1", Some(Private)),
            ("172.31.255.255", Some(Private)),
            ("192.168.1.10", Some(Private)),
            ("169.254.169.254", Some(LinkLocal)),
            ("169.254.0.1", Some(LinkLocal)),
            ("100.64.0.1", Some(Cgnat)),
            ("100.127.255.255", Some(Cgnat)),
            ("224.0.0.1", Some(Multicast)),
            ("239.255.255.250", Some(Multicast)),
            ("0.0.0.0", Some(Unspecified)),
            ("0.1.2.3", Some(Unspecified)),
            ("255.255.255.255", Some(Broadcast)),
            ("::1", Some(Loopback)),
            ("::", Some(Unspecified)),
            ("fe80::1", Some(LinkLocal)),
            ("febf::1", Some(LinkLocal)),
            ("fc00::1", Some(UniqueLocal)),
            ("fd12:3456::1", Some(UniqueLocal)),
            ("fec0::1", Some(UniqueLocal)),
            ("ff02::1", Some(Multicast)),
            // IPv4 addresses smuggled inside IPv6 forms get the IPv4 verdict.
            ("::ffff:127.0.0.1", Some(Loopback)),
            ("::ffff:10.1.2.3", Some(Private)),
            ("::ffff:169.254.169.254", Some(LinkLocal)),
            ("::ffff:93.184.215.14", None),
            ("::127.0.0.1", Some(Loopback)),
            ("::192.168.0.1", Some(Private)),
            ("64:ff9b::7f00:1", Some(Loopback)),
            ("64:ff9b::a9fe:a9fe", Some(LinkLocal)),
            ("64:ff9b::5db8:d70e", None),
        ];
        for (literal, expected) in cases {
            let addr: IpAddr = literal.parse().expect("test literals are valid addresses");
            assert_eq!(classify(addr), *expected, "{literal}");
        }
    }
}

//! Acceptance tests for the outbound fetch guard against a live local HTTP
//! server.
//!
//! Every test that needs a socket lives in this one binary and shares one
//! server on `127.0.0.1:${POD_TEST_HTTP_PORT:-3007}`, because sandboxed
//! development environments can only bind a few fixed ports (see
//! CLAUDE.md). The tests run concurrently against it.
//!
//! Host names resolve through [`TestLookup`], which maps every `*.test` name
//! to 127.0.0.1. `feeds.test` is allowlisted in most tests and plays the
//! public feed host; `internal.test` is never allowlisted and plays an
//! internal one.

use std::collections::HashMap;
use std::future::Future;
use std::io::{self, Write};
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pod_server::fetch::{
    Allowlist, BlockReason, BlockedAddress, FetchError, FetchPolicy, FetchResponse, Fetcher,
    Lookup, MAX_BODY_BYTES, MAX_REDIRECTS,
};
use url::Url;

// ==============================================================================
// Shared server
// ==============================================================================

fn port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| {
        std::env::var("POD_TEST_HTTP_PORT")
            .ok()
            .filter(|port| !port.is_empty())
            .map(|port| port.parse().expect("POD_TEST_HTTP_PORT is a port number"))
            .unwrap_or(3007)
    })
}

/// Requests per `/count/{name}` path, so tests can tell whether a refused
/// fetch reached the server.
fn hits() -> &'static Mutex<HashMap<String, usize>> {
    static HITS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    HITS.get_or_init(Mutex::default)
}

fn hit_count(name: &str) -> usize {
    hits()
        .lock()
        .expect("no test panics while holding the lock")
        .get(name)
        .copied()
        .unwrap_or(0)
}

/// Starts the server once, on its own thread and runtime so it outlives each
/// test's runtime.
fn ensure_server() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("the test runtime builds");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port()))
                    .await
                    .expect("the test HTTP port is free (set POD_TEST_HTTP_PORT)");
                ready_tx
                    .send(())
                    .expect("the test thread waits for readiness");
                axum::serve(listener, routes())
                    .await
                    .expect("the test server runs until the process exits");
            });
        });
        ready_rx
            .recv()
            .expect("the server thread reports readiness");
    });
}

fn routes() -> Router {
    Router::new()
        .route("/feed", get(feed))
        .route("/echo", get(echo))
        .route("/status/{code}", get(status))
        .route("/redirect", get(redirect))
        .route("/hop", get(hop))
        .route("/count/{name}", get(count))
        .route("/sized/{bytes}", get(sized))
        .route("/endless", get(endless))
        .route("/slow", get(slow))
}

async fn feed(headers: HeaderMap) -> Response {
    if headers.get(header::IF_NONE_MATCH) == Some(&HeaderValue::from_static("\"v1\"")) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (header::ETAG, "\"v1\""),
            (header::LAST_MODIFIED, "Sat, 24 Oct 2026 10:00:00 GMT"),
        ],
        "<rss/>",
    )
        .into_response()
}

/// Echoes the request headers the guard controls.
async fn echo(headers: HeaderMap) -> String {
    let get = |name| {
        headers
            .get(name)
            .and_then(|value: &HeaderValue| value.to_str().ok())
            .unwrap_or("-")
            .to_owned()
    };
    format!(
        "user-agent={}\nauthorization={}\n",
        get(header::USER_AGENT),
        get(header::AUTHORIZATION)
    )
}

async fn status(Path(code): Path<u16>) -> Response {
    let status = StatusCode::from_u16(code).expect("tests request valid statuses");
    ([(header::RETRY_AFTER, "120")], status).into_response()
}

/// `/redirect?to=<url>&code=<status>` redirects to `to` with `code` (302 by
/// default).
async fn redirect(Query(query): Query<HashMap<String, String>>) -> Response {
    let code = query.get("code").map_or(302, |code| {
        code.parse().expect("tests pass numeric redirect codes")
    });
    let status = StatusCode::from_u16(code).expect("tests pass valid redirect codes");
    (status, [(header::LOCATION, query["to"].clone())]).into_response()
}

/// `/hop?codes=301,302` answers with the first code and a relative redirect
/// to `/hop?codes=302`, and so on until the list is empty, then to `/feed`.
async fn hop(Query(query): Query<HashMap<String, String>>) -> Response {
    let codes = query.get("codes").map_or("", String::as_str);
    let (first, rest) = codes.split_once(',').unwrap_or((codes, ""));
    let status = StatusCode::from_u16(first.parse().expect("tests pass numeric codes"))
        .expect("tests pass valid redirect codes");
    let location = if rest.is_empty() {
        "/feed".to_owned()
    } else {
        format!("/hop?codes={rest}")
    };
    (status, [(header::LOCATION, location)]).into_response()
}

async fn count(Path(name): Path<String>) -> &'static str {
    *hits()
        .lock()
        .expect("no test panics while holding the lock")
        .entry(name)
        .or_default() += 1;
    "<rss/>"
}

async fn sized(Path(bytes): Path<usize>) -> Vec<u8> {
    vec![b'x'; bytes]
}

/// A chunked body that never ends, so only an enforced cap stops the fetch.
async fn endless() -> Body {
    let chunk = bytes::Bytes::from(vec![b'x'; 64 * 1024]);
    Body::from_stream(futures_util::stream::repeat_with(move || {
        Ok::<_, io::Error>(chunk.clone())
    }))
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(10)).await;
    "<rss/>"
}

// ==============================================================================
// Fetcher setup
// ==============================================================================

/// Maps `*.test` and `localhost` to 127.0.0.1, except `rebind.test`, which
/// answers 127.0.0.1 first and 127.0.0.2 after that, `refused.test`, which
/// maps to 127.0.0.2 where nothing listens, and `missing.test`, which
/// doesn't resolve.
#[derive(Default)]
struct TestLookup {
    rebind_lookups: AtomicUsize,
}

impl Lookup for TestLookup {
    fn lookup(&self, host: &str) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>> {
        let answer = match host {
            "missing.test" => Err(io::Error::new(io::ErrorKind::NotFound, "no such host")),
            "refused.test" => Ok(vec![IpAddr::from([127, 0, 0, 2])]),
            "rebind.test" => match self.rebind_lookups.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(vec![IpAddr::from([127, 0, 0, 1])]),
                _ => Ok(vec![IpAddr::from([127, 0, 0, 2])]),
            },
            _ => Ok(vec![IpAddr::from([127, 0, 0, 1])]),
        };
        Box::pin(async move { answer })
    }
}

fn policy(allowlist: &[&str]) -> FetchPolicy {
    let mut policy =
        FetchPolicy::new(Allowlist::parse(allowlist).expect("test allowlists are valid"));
    policy.allowed_ports = vec![port()];
    policy
}

fn fetcher(policy: FetchPolicy) -> Fetcher {
    fetcher_with(policy, Arc::new(TestLookup::default()))
}

fn fetcher_with(policy: FetchPolicy, lookup: Arc<dyn Lookup>) -> Fetcher {
    ensure_server();
    Fetcher::with_lookup(policy, "https://pod.example.org", lookup).expect("the HTTP client builds")
}

/// `http://<host>:<port><path>`, with `host` possibly carrying credentials.
fn url(host: &str, path: &str) -> Url {
    Url::parse(&format!("http://{host}:{}{path}", port())).expect("test URLs are valid")
}

async fn fetch(fetcher: &Fetcher, url: &Url) -> Result<FetchResponse, FetchError> {
    fetcher.get(url, HeaderMap::new()).await
}

fn assert_blocked(result: &Result<FetchResponse, FetchError>, reason: BlockReason) {
    assert!(
        matches!(result, Err(FetchError::Blocked(BlockedAddress { reason: r, .. })) if *r == reason),
        "expected a {reason} block, got {result:?}"
    );
}

fn body(response: &FetchResponse) -> &str {
    std::str::from_utf8(&response.body).expect("test bodies are UTF-8")
}

// ==============================================================================
// Tests
// ==============================================================================

#[tokio::test]
async fn fetch_returns_status_cache_headers_and_body() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    let response = fetch(&fetcher, &url("feeds.test", "/feed"))
        .await
        .expect("the allowlisted host is reachable");
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.headers[header::ETAG], "\"v1\"");
    assert_eq!(
        response.headers[header::LAST_MODIFIED],
        "Sat, 24 Oct 2026 10:00:00 GMT"
    );
    assert_eq!(body(&response), "<rss/>");
    assert_eq!(response.final_url, url("feeds.test", "/feed"));
    assert_eq!(response.permanent_redirect, None);
}

#[tokio::test]
async fn user_agent_names_version_and_base_url() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    let response = fetch(&fetcher, &url("feeds.test", "/echo"))
        .await
        .expect("the allowlisted host is reachable");
    let expected = format!(
        "user-agent=pod/{} (+https://pod.example.org)\n",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        body(&response).starts_with(&expected),
        "{}",
        body(&response)
    );
}

#[tokio::test]
async fn caller_headers_make_conditional_requests() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    let mut headers = HeaderMap::new();
    headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("\"v1\""));
    let response = fetcher
        .get(&url("feeds.test", "/feed"), headers)
        .await
        .expect("304 is a response, not an error");
    assert_eq!(response.status, StatusCode::NOT_MODIFIED);
    assert!(response.body.is_empty());
}

#[tokio::test]
async fn error_statuses_are_responses() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    for code in [410, 429, 503, 500] {
        let response = fetch(&fetcher, &url("feeds.test", &format!("/status/{code}")))
            .await
            .expect("an error status is a response, not an error");
        assert_eq!(response.status.as_u16(), code);
        assert_eq!(response.headers[header::RETRY_AFTER], "120");
    }
}

#[tokio::test]
async fn loopback_literal_needs_the_allowlist() {
    let target = url("127.0.0.1", "/count/loopback-literal");
    let refused = fetch(&fetcher(policy(&[])), &target).await;
    assert_blocked(&refused, BlockReason::Loopback);
    assert_eq!(hit_count("loopback-literal"), 0);

    let allowed = fetch(&fetcher(policy(&["127.0.0.1"])), &target)
        .await
        .expect("an allowlisted address is reachable");
    assert_eq!(allowed.status, StatusCode::OK);
    assert_eq!(hit_count("loopback-literal"), 1);
}

#[tokio::test]
async fn lan_host_name_needs_the_allowlist() {
    let target = url("nas.test", "/count/lan-host");
    let refused = fetch(&fetcher(policy(&[])), &target).await;
    assert_blocked(&refused, BlockReason::Loopback);
    assert_eq!(hit_count("lan-host"), 0);

    let allowed = fetch(&fetcher(policy(&["nas.test"])), &target)
        .await
        .expect("an allowlisted host is reachable");
    assert_eq!(allowed.status, StatusCode::OK);
    assert_eq!(hit_count("lan-host"), 1);
}

#[tokio::test]
async fn system_resolver_refuses_localhost_unless_allowlisted() {
    ensure_server();
    let system = |allowlist| {
        Fetcher::new(policy(allowlist), "https://pod.example.org").expect("the client builds")
    };
    let target = url("localhost", "/feed");
    let refused = fetch(&system(&[]), &target).await;
    assert_blocked(&refused, BlockReason::Loopback);

    let allowed = fetch(&system(&["localhost"]), &target)
        .await
        .expect("allowlisted localhost is reachable");
    assert_eq!(allowed.status, StatusCode::OK);
}

#[tokio::test]
async fn redirect_from_public_to_internal_is_refused() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    for (target, reason) in [
        (
            url("127.0.0.1", "/count/redirect-literal"),
            BlockReason::Loopback,
        ),
        (
            url("internal.test", "/count/redirect-name"),
            BlockReason::Loopback,
        ),
        (
            url("[::ffff:127.0.0.1]", "/count/redirect-mapped"),
            BlockReason::Loopback,
        ),
        (
            url("169.254.169.254", "/latest/meta-data/"),
            BlockReason::LinkLocal,
        ),
        (url("10.0.0.1", "/admin"), BlockReason::Private),
    ] {
        let mut start = url("feeds.test", "/redirect");
        start.query_pairs_mut().append_pair("to", target.as_str());
        let result = fetch(&fetcher, &start).await;
        assert_blocked(&result, reason);
    }
    for name in ["redirect-literal", "redirect-name", "redirect-mapped"] {
        assert_eq!(hit_count(name), 0, "{name}");
    }
}

#[tokio::test]
async fn redirect_to_another_scheme_or_port_is_refused() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    let mut start = url("feeds.test", "/redirect");
    start
        .query_pairs_mut()
        .append_pair("to", "file:///etc/passwd");
    assert!(matches!(
        fetch(&fetcher, &start).await,
        Err(FetchError::UnsupportedScheme(scheme)) if scheme == "file"
    ));

    let mut start = url("feeds.test", "/redirect");
    start
        .query_pairs_mut()
        .append_pair("to", "http://internal.test:6379/");
    assert!(matches!(
        fetch(&fetcher, &start).await,
        Err(FetchError::PortNotAllowed(6379))
    ));
}

// ==============================================================================
// Ports of allowlisted hosts
// ==============================================================================

// These tests use the production policy, which allows only ports 80 and 443
// to hosts outside the allowlist, so the test server's port is a
// non-standard one.

fn production_policy(allowlist: &[&str]) -> FetchPolicy {
    FetchPolicy::new(Allowlist::parse(allowlist).expect("test allowlists are valid"))
}

#[tokio::test]
async fn allowlisted_lan_host_may_use_any_port() {
    for (host, entry) in [("nas.test", "nas.test"), ("127.0.0.1", "127.0.0.0/8")] {
        let name = format!("any-port-{host}");
        let target = url(host, &format!("/count/{name}"));

        let refused = fetch(&fetcher(production_policy(&[])), &target).await;
        assert!(
            matches!(refused, Err(FetchError::PortNotAllowed(p)) if p == port()),
            "{host}: {refused:?}"
        );
        assert_eq!(hit_count(&name), 0, "{host}");

        let allowed = fetch(&fetcher(production_policy(&[entry])), &target)
            .await
            .expect("an allowlisted host is reachable on any port");
        assert_eq!(allowed.status, StatusCode::OK, "{host}");
        assert_eq!(hit_count(&name), 1, "{host}");
    }
}

#[tokio::test]
async fn hosts_the_allowlist_does_not_name_keep_the_port_rule() {
    // `feeds.test` plays a public host; `nas.test` resolves into the
    // allowlisted network but is a name, so only its address is opened.
    for (host, allowlist) in [
        ("feeds.test", &["nas.test"][..]),
        ("nas.test", &["127.0.0.0/8"][..]),
    ] {
        let name = format!("port-rule-{host}");
        let target = url(host, &format!("/count/{name}"));
        let result = fetch(&fetcher(production_policy(allowlist)), &target).await;
        assert!(
            matches!(result, Err(FetchError::PortNotAllowed(p)) if p == port()),
            "{host}: {result:?}"
        );
        assert_eq!(hit_count(&name), 0, "{host}");
    }
}

#[tokio::test]
async fn redirect_from_allowlisted_host_to_another_port_needs_the_allowlist_too() {
    let fetcher = fetcher(production_policy(&["nas.test"]));
    let mut start = url("nas.test", "/redirect");
    start
        .query_pairs_mut()
        .append_pair("to", url("feeds.test", "/count/redirect-port").as_str());
    let result = fetch(&fetcher, &start).await;
    assert!(
        matches!(result, Err(FetchError::PortNotAllowed(p)) if p == port()),
        "{result:?}"
    );
    assert_eq!(hit_count("redirect-port"), 0);
}

/// `rebind.test` answers 127.0.0.1 (allowlisted) on the first lookup and
/// 127.0.0.2 (loopback, not allowlisted, and not listening) on every later
/// one. A guard that checked one answer and let the connection resolve again
/// would connect to 127.0.0.2 and fail; this one connects to the address it
/// checked.
#[tokio::test]
async fn connection_uses_the_checked_answer() {
    let lookup = Arc::new(TestLookup::default());
    let fetcher = fetcher_with(policy(&["127.0.0.1"]), lookup.clone());
    let response = fetch(&fetcher, &url("rebind.test", "/count/rebind"))
        .await
        .expect("the first, allowlisted answer is used for the connection");
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(hit_count("rebind"), 1);
    assert_eq!(lookup.rebind_lookups.load(Ordering::SeqCst), 1);

    // A new connection resolves again, and the rebound answer is refused.
    let fresh = fetcher_with(policy(&["127.0.0.1"]), lookup.clone());
    let result = fetch(&fresh, &url("rebind.test", "/count/rebind")).await;
    assert_blocked(&result, BlockReason::Loopback);
    assert_eq!(hit_count("rebind"), 1);
}

#[tokio::test]
async fn permanent_redirects_are_reported() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    let hop = |codes: &str| url("feeds.test", &format!("/hop?codes={codes}"));
    for (codes, permanent) in [
        ("302", None),
        ("307,303", None),
        ("301", Some(url("feeds.test", "/feed"))),
        ("308,302", Some(hop("302"))),
        ("302,301", Some(url("feeds.test", "/feed"))),
        ("301,302,308,307", Some(hop("307"))),
    ] {
        let response = fetch(&fetcher, &hop(codes))
            .await
            .expect("the redirect chain ends at the feed");
        assert_eq!(response.status, StatusCode::OK, "{codes}");
        assert_eq!(response.final_url, url("feeds.test", "/feed"), "{codes}");
        assert_eq!(response.permanent_redirect, permanent, "{codes}");
    }
}

#[tokio::test]
async fn redirect_limit() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    let chain = |hops: usize| {
        let codes = vec!["302"; hops].join(",");
        url("feeds.test", &format!("/hop?codes={codes}"))
    };
    let response = fetch(&fetcher, &chain(MAX_REDIRECTS))
        .await
        .expect("five redirects are allowed");
    assert_eq!(response.status, StatusCode::OK);

    let result = fetch(&fetcher, &chain(MAX_REDIRECTS + 1)).await;
    assert!(
        matches!(result, Err(FetchError::TooManyRedirects)),
        "{result:?}"
    );
}

#[tokio::test]
async fn body_cap() {
    let fetcher = fetcher(policy(&["feeds.test"]));
    let at_cap = fetch(
        &fetcher,
        &url("feeds.test", &format!("/sized/{MAX_BODY_BYTES}")),
    )
    .await
    .expect("a body of exactly the cap is accepted");
    assert_eq!(at_cap.body.len(), MAX_BODY_BYTES);

    let declared = fetch(
        &fetcher,
        &url("feeds.test", &format!("/sized/{}", MAX_BODY_BYTES + 1)),
    )
    .await;
    assert!(
        matches!(declared, Err(FetchError::BodyTooLarge)),
        "{declared:?}"
    );

    let endless = fetch(&fetcher, &url("feeds.test", "/endless")).await;
    assert!(
        matches!(endless, Err(FetchError::BodyTooLarge)),
        "{endless:?}"
    );
}

#[tokio::test]
async fn timeout_covers_the_whole_fetch() {
    let mut policy = policy(&["feeds.test"]);
    policy.timeout = Duration::from_millis(300);
    let result = fetch(&fetcher(policy), &url("feeds.test", "/slow")).await;
    assert!(matches!(result, Err(FetchError::Timeout)), "{result:?}");
}

// ==============================================================================
// Credentials
// ==============================================================================

/// Captures every log line in this process, including reqwest's and
/// hyper's, at trace level.
fn captured_logs() -> &'static Mutex<Vec<u8>> {
    static LOGS: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(|| LogWriter)
            .init();
        Mutex::default()
    })
}

struct LogWriter;

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        captured_logs()
            .lock()
            .expect("no test panics while holding the lock")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const USERNAME: &str = "alice";
const PASSWORD: &str = "s3cret%21pw";
/// Base64 of `alice:s3cret!pw`: the password is percent-decoded first.
const BASIC_CREDENTIALS: &str = "YWxpY2U6czNjcmV0IXB3";

fn with_credentials(host: &str, path: &str) -> Url {
    url(&format!("{USERNAME}:{PASSWORD}@{host}"), path)
}

#[tokio::test]
async fn credentials_go_out_as_basic_auth_and_stay_out_of_logs() {
    captured_logs();
    let mut policy = policy(&["feeds.test", "nas.test", "refused.test"]);
    policy.timeout = Duration::from_millis(500);
    let fetcher = fetcher(policy);
    let mut debug_output = String::new();

    let direct = fetch(&fetcher, &with_credentials("feeds.test", "/echo"))
        .await
        .expect("the allowlisted host is reachable");
    assert!(
        body(&direct).contains(&format!("authorization=Basic {BASIC_CREDENTIALS}\n")),
        "{}",
        body(&direct)
    );
    debug_output += &format!("{direct:?}\n");

    // Same origin keeps the credentials, and the reported URL carries them
    // so the stored feed URL still authenticates.
    let mut same_origin = with_credentials("feeds.test", "/redirect");
    same_origin
        .query_pairs_mut()
        .append_pair("to", "/echo")
        .append_pair("code", "301");
    let response = fetch(&fetcher, &same_origin)
        .await
        .expect("the redirect target is reachable");
    assert!(
        body(&response).contains(BASIC_CREDENTIALS),
        "{}",
        body(&response)
    );
    assert_eq!(
        response.permanent_redirect,
        Some(with_credentials("feeds.test", "/echo"))
    );
    debug_output += &format!("{response:?}\n");

    // Another origin doesn't get them.
    let mut cross_origin = with_credentials("feeds.test", "/redirect");
    cross_origin
        .query_pairs_mut()
        .append_pair("to", url("nas.test", "/echo").as_str());
    let response = fetch(&fetcher, &cross_origin)
        .await
        .expect("the redirect target is reachable");
    assert!(
        body(&response).contains("authorization=-\n"),
        "{}",
        body(&response)
    );

    for failing in [
        with_credentials("internal.test", "/feed"),
        with_credentials("missing.test", "/feed"),
        with_credentials("feeds.test", "/slow"),
        with_credentials("127.0.0.1", "/feed"),
        with_credentials("refused.test", "/feed"),
    ] {
        let error = fetch(&fetcher, &failing)
            .await
            .expect_err("the fetch fails");
        let mut rendered = format!("{error}\n{error:?}\n");
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            rendered += &format!("{cause}\n{cause:?}\n");
            source = cause.source();
        }
        // Errors leave out the whole URL, not just its credentials. A blocked
        // literal IP host still shows up as the refused address.
        if let Some(url::Host::Domain(host)) = failing.host() {
            assert!(!rendered.contains(host), "{host} in {rendered}");
        }
        debug_output += &rendered;
    }

    let logs = String::from_utf8(
        captured_logs()
            .lock()
            .expect("no test panics while holding the lock")
            .clone(),
    )
    .expect("log output is UTF-8");
    assert!(!logs.is_empty(), "the subscriber captured reqwest's logs");
    for secret in [USERNAME, "s3cret", BASIC_CREDENTIALS] {
        assert!(!logs.contains(secret), "{secret} leaked into logs:\n{logs}");
        assert!(
            !debug_output.contains(secret),
            "{secret} leaked into Display/Debug output:\n{debug_output}"
        );
    }
}

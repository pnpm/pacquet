//! Tests for [`super`]'s proxy plumbing.
//!
//! Mirrors the describe blocks in pnpm v11's
//! [`network/fetch/test/dispatcher.test.ts`](https://github.com/pnpm/pnpm/blob/94240bc046/network/fetch/test/dispatcher.test.ts)
//! that don't require a real proxy listener:
//!
//! * `HTTP proxy` — per-URL routing, basic-auth decoding, scheme bypass.
//! * `SOCKS proxy` — routing decision (live-network case skipped, same as
//!   upstream).
//! * `noProxy` — reverse-dot-segment match, bypass-all literal.
//! * `Invalid proxy URL` — `ERR_PNPM_INVALID_PROXY`.
//!
//! The one HTTP integration test stands up a [`mockito`] server playing
//! the role of an HTTP proxy and asserts the request arrives with an
//! absolute-form URI and a decoded `Proxy-Authorization` header.

use super::{
    NoProxyMatcher, NoProxySetting, ProxyConfig, ProxyError, ThrottledClient, parse_proxy_url,
};
use crate::proxy::{percent_decode_str, strip_userinfo};
use reqwest::Url;

fn list(entries: &[&str]) -> NoProxySetting {
    NoProxySetting::List(entries.iter().map(|s| (*s).to_string()).collect())
}

#[test]
fn no_proxy_matcher_reverse_dot_match() {
    let m = NoProxyMatcher::from(Some(&list(&["npmjs.org"])));
    // The matcher state is the same across every probe; logging it
    // once per test makes a failure diagnosable without rerunning.
    eprintln!("matcher={m:?}");
    for (host, expected) in [
        ("npmjs.org", true),
        ("registry.npmjs.org", true),
        ("foo.bar.npmjs.org", true),
        ("evilnpmjs.org", false),
        ("org", false),
    ] {
        let got = m.matches_host(host);
        assert_eq!(got, expected, "host={host}: expected match={expected}, got={got}");
    }
}

#[test]
fn no_proxy_matcher_empty_entries_never_match() {
    // Trailing/leading commas in `.npmrc` already get filtered in the
    // config layer's `parse_no_proxy`, but a malformed `List(vec![""])`
    // must still fail to match — defense in depth at the matcher.
    let m = NoProxyMatcher::from(Some(&list(&[""])));
    let got = m.matches_host("anything.example");
    assert!(!got, "matcher={m:?} host=anything.example expected miss, got match");
}

#[test]
fn no_proxy_matcher_multiple_entries() {
    let m = NoProxyMatcher::from(Some(&list(&["npmjs.org", "internal.example"])));
    eprintln!("matcher={m:?}");
    for (host, expected) in
        [("registry.npmjs.org", true), ("ci.internal.example", true), ("public.example", false)]
    {
        let got = m.matches_host(host);
        assert_eq!(got, expected, "host={host}: expected={expected}, got={got}");
    }
}

#[test]
fn no_proxy_bypass_short_circuits_every_host() {
    let m = NoProxyMatcher::from(Some(&NoProxySetting::Bypass));
    eprintln!("matcher={m:?}");
    for host in ["any.host", ""] {
        let got = m.matches_host(host);
        assert!(got, "host={host:?}: bypass must match every host, got miss");
    }
}

#[test]
fn no_proxy_none_matches_nothing() {
    let m = NoProxyMatcher::from(None);
    let got = m.matches_host("registry.npmjs.org");
    assert!(!got, "matcher={m:?}: None setting must never match");
}

#[test]
fn parse_proxy_url_auto_prefixes_missing_scheme() {
    // pnpm-parity: `proxy.example:8080` is treated as
    // `http://proxy.example:8080`.
    let url = parse_proxy_url("proxy.example:8080").expect("parses with retry");
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("proxy.example"));
    assert_eq!(url.port(), Some(8080));
}

#[test]
fn parse_proxy_url_keeps_existing_scheme() {
    let url = parse_proxy_url("https://proxy.example:8080").expect("parses");
    assert_eq!(url.scheme(), "https");
}

#[test]
fn parse_proxy_url_socks_schemes_pass_through() {
    // pnpm honors socks4, socks4a, socks5 (dispatcher.ts:124-132).
    // Routing happens elsewhere; here we only assert the URL parses.
    for scheme in ["socks4", "socks4a", "socks5"] {
        let url =
            parse_proxy_url(&format!("{scheme}://socksproxy.example:1080")).expect("socks parses");
        assert_eq!(url.scheme(), scheme);
    }
}

#[test]
fn parse_proxy_url_invalid_returns_invalid_proxy_error() {
    // `://` is malformed regardless of which scheme is prefixed.
    let err = parse_proxy_url("://broken").expect_err("malformed value must error");
    eprintln!("err={err:?}");
    match &err {
        ProxyError::InvalidProxy { url, .. } => assert_eq!(url, "://broken"),
    }
    // Diagnostic code matches upstream `ERR_PNPM_INVALID_PROXY`.
    let code = miette::Diagnostic::code(&err).expect("code() set");
    assert_eq!(code.to_string(), "ERR_PNPM_INVALID_PROXY");
}

#[test]
fn percent_decode_handles_common_escapes() {
    assert_eq!(percent_decode_str("p%40ss"), "p@ss", "%40 → @");
    assert_eq!(percent_decode_str("user%20name"), "user name");
    assert_eq!(percent_decode_str("plain"), "plain");
    assert_eq!(
        percent_decode_str("bad-%ZZ-escape"),
        "bad-%ZZ-escape",
        "invalid hex passes through",
    );
}

#[test]
fn strip_userinfo_decodes_user_and_password() {
    let url = Url::parse("http://us%40er:p%40ss@proxy.example:8080").expect("parse");
    let (clean, auth) = strip_userinfo(url);
    assert_eq!(clean.as_str(), "http://proxy.example:8080/");
    let (user, pass) = auth.expect("userinfo present");
    assert_eq!(user, "us@er", "user percent-decoded");
    assert_eq!(pass, "p@ss", "password percent-decoded");
}

#[test]
fn strip_userinfo_returns_none_when_absent() {
    let url = Url::parse("http://proxy.example:8080").expect("parse");
    let (clean, auth) = strip_userinfo(url.clone());
    assert_eq!(clean, url);
    let is_none = auth.is_none();
    assert!(is_none, "auth={auth:?}: expected None on URL without userinfo");
}

#[test]
fn for_installs_with_empty_proxy_config_builds() {
    // The legacy `new_for_installs` is now a wrapper around this — assert
    // the default `ProxyConfig` round-trips without error.
    ThrottledClient::for_installs(&ProxyConfig::default()).expect("empty proxy is valid");
}

#[test]
fn for_installs_with_valid_proxy_url_builds() {
    let proxy = ProxyConfig {
        https_proxy: Some("http://proxy.example:8080".into()),
        http_proxy: Some("http://proxy.example:8080".into()),
        no_proxy: None,
    };
    ThrottledClient::for_installs(&proxy).expect("valid proxy URLs build");
}

#[test]
fn for_installs_with_invalid_proxy_url_errors() {
    let proxy =
        ProxyConfig { https_proxy: Some("://nonsense".into()), http_proxy: None, no_proxy: None };
    let err = ThrottledClient::for_installs(&proxy).expect_err("must error");
    eprintln!("err={err:?}");
    let is_invalid = matches!(err, ProxyError::InvalidProxy { .. });
    assert!(is_invalid, "err={err:?}: expected ProxyError::InvalidProxy");
}

#[test]
fn for_installs_with_socks_proxy_url_builds() {
    // Smoke test that the `socks` reqwest feature is wired correctly —
    // a socks URL must not be rejected at parse time, and the client
    // must build.
    let proxy = ProxyConfig {
        https_proxy: Some("socks5://socksproxy.example:1080".into()),
        http_proxy: None,
        no_proxy: None,
    };
    ThrottledClient::for_installs(&proxy).expect("socks proxy URL builds");
}

#[test]
fn for_installs_no_proxy_bypass_does_not_block_build() {
    let proxy = ProxyConfig {
        https_proxy: Some("http://proxy.example:8080".into()),
        http_proxy: None,
        no_proxy: Some(NoProxySetting::Bypass),
    };
    ThrottledClient::for_installs(&proxy).expect("bypass + proxy URL builds");
}

/// End-to-end check that `for_installs` actually routes HTTP traffic
/// through the configured proxy. We stand a `mockito` server up as an
/// upstream HTTP proxy: when a client is configured with `http_proxy =
/// <mockito_url>` and asked to fetch a different target URL, the request
/// arrives at the mockito server bearing the absolute-form URI in its
/// request line and the matching `Proxy-Authorization` header from the
/// percent-decoded userinfo.
#[tokio::test]
async fn mockito_integration_http_proxy_forwards_request_with_basic_auth() {
    let mut proxy_server = mockito::Server::new_async().await;
    // The mock matches *any* path because reqwest's HTTP-proxy mode
    // sends the request line with the absolute-form URI of the target
    // (RFC 9112 §3.2.2). We pin auth & method instead.
    let mock = proxy_server
        .mock("GET", mockito::Matcher::Any)
        .match_header("proxy-authorization", "Basic dXNlckBuYW1lOnBAc3M=")
        .with_status(200)
        .with_body("ok")
        .expect(1)
        .create_async()
        .await;

    let proxy_url = proxy_server.url();
    // `user@name:p@ss` percent-encoded → `user%40name:p%40ss`; the
    // network layer percent-decodes both halves to `user@name` and
    // `p@ss` and base64-encodes the pair as `dXNlckBuYW1lOnBAc3M=` —
    // the value the mock matches above.
    let with_auth = proxy_url.replacen("//", "//user%40name:p%40ss@", 1);
    let cfg = ProxyConfig { https_proxy: None, http_proxy: Some(with_auth), no_proxy: None };
    let client = ThrottledClient::for_installs(&cfg).expect("valid proxy");
    let guard = client.acquire().await;
    let resp = guard.get("http://target.example/anything").send().await.expect("proxied request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "ok");
    mock.assert_async().await;
}

#[tokio::test]
async fn mockito_integration_no_proxy_bypasses_proxy() {
    // Sanity check the bypass path: with `NoProxySetting::Bypass`, the
    // client must not consult the proxy at all. We register the proxy
    // mock with `expect(0)` and rely on `mockito`'s drop-time assertion.
    let mut proxy_server = mockito::Server::new_async().await;
    let proxy_mock = proxy_server
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .with_status(500)
        .create_async()
        .await;

    let mut target_server = mockito::Server::new_async().await;
    let target_path = "/direct";
    let target_mock = target_server
        .mock("GET", target_path)
        .expect(1)
        .with_status(200)
        .with_body("direct")
        .create_async()
        .await;

    let cfg = ProxyConfig {
        https_proxy: None,
        http_proxy: Some(proxy_server.url()),
        no_proxy: Some(NoProxySetting::Bypass),
    };
    let client = ThrottledClient::for_installs(&cfg).expect("valid proxy");
    let guard = client.acquire().await;
    let url = format!("{}{}", target_server.url(), target_path);
    let resp = guard.get(&url).send().await.expect("direct request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "direct");
    proxy_mock.assert_async().await;
    target_mock.assert_async().await;
}

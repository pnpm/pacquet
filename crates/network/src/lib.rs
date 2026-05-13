mod auth;
mod proxy;
#[cfg(test)]
mod tests;
mod tls;

pub use auth::{AuthHeaders, base64_encode, nerf_dart};
pub use proxy::{NoProxySetting, ProxyConfig, ProxyError};
pub use tls::{TlsConfig, TlsError};

use proxy::{NoProxyMatcher, parse_proxy_url, strip_userinfo};
use reqwest::{
    Certificate, Client, Identity, Proxy,
    header::{HeaderMap, HeaderValue, USER_AGENT},
};
use std::{num::NonZeroUsize, ops::Deref, sync::Arc, time::Duration};
use tokio::sync::{Semaphore, SemaphorePermit};

/// Default `User-Agent` pacquet sends on every request made by the
/// install client — registry metadata fetches and tarball downloads
/// alike, including tarball URLs that point at non-registry hosts.
///
/// Identical to pnpm v11's
/// [`network/fetch/src/fetchFromRegistry.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/network/fetch/src/fetchFromRegistry.ts#L9):
/// the literal string `pnpm`. A default `reqwest::Client` sends *no*
/// User-Agent at all, which some registry CDNs and corporate WAFs
/// treat as a bot signature and either block at the edge or terminate
/// mid-handshake (surfacing as a generic "error sending request for
/// url" with no body to look at).
///
/// We deliberately send `pnpm` rather than `pacquet/<version>` so
/// any UA-keyed allow / rate-limit rule that lets pnpm through also
/// lets pacquet through. Pacquet is a port of pnpm; behavioural
/// parity, including what the registry sees on the wire, is the
/// goal.
const DEFAULT_USER_AGENT: &str = "pnpm";

/// Wrapper around [`Client`] with concurrent request limit enforced by the [`Semaphore`] mechanism.
#[derive(Debug)]
pub struct ThrottledClient {
    semaphore: Semaphore,
    client: Client,
}

/// RAII guard returned from [`ThrottledClient::acquire`]. Holds a
/// semaphore permit alongside a reference to the underlying
/// [`Client`]; the permit is released when the guard is dropped.
///
/// The guard derefs to [`Client`] so callers can chain
/// `guard.get(url).send().await?.json().await?` (or any other
/// reqwest method) directly. **Holding the guard across the body
/// await is the point of the API.** A request's socket FD lives
/// from `connect` all the way through body streaming; dropping the
/// permit when `.send()` returns (right after headers arrive, with
/// the body still pending) means the semaphore stops bounding the
/// real concurrent socket count. Under `try_join_all` fan-out the
/// next batch of permits then `connect()` while previous bodies are
/// still draining, and the per-process FD count overruns the
/// platform limit — surfacing as `EMFILE` "too many open files".
pub struct ThrottledClientGuard<'a> {
    _permit: SemaphorePermit<'a>,
    client: &'a Client,
}

impl<'a> Deref for ThrottledClientGuard<'a> {
    type Target = Client;

    fn deref(&self) -> &Client {
        self.client
    }
}

impl ThrottledClient {
    /// Acquire a permit and return a guard granting access to the
    /// underlying [`Client`]. The permit is released when the guard
    /// is dropped, so callers control how long the request "counts"
    /// against [`default_network_concurrency`] — typically the full
    /// `send + body-consume` lifetime, not just `.send()`.
    pub async fn acquire(&self) -> ThrottledClientGuard<'_> {
        let permit =
            self.semaphore.acquire().await.expect("semaphore shouldn't have been closed this soon");
        ThrottledClientGuard { _permit: permit, client: &self.client }
    }

    /// Construct the default throttled client used for real installs.
    ///
    /// Network topology is ported from pnpm v11's
    /// `network/fetch/src/dispatcher.ts` (see #280):
    ///
    /// * **HTTP/1.1 only.** A default `reqwest::Client` upgrades to
    ///   HTTP/2 via ALPN whenever the registry advertises it
    ///   (registry.npmjs.org does). Pnpm explicitly disables this
    ///   upstream after benchmarking — multiplexing many tarball
    ///   streams over 1-2 TCP connections sharing one congestion
    ///   window was slower than opening ~50 independent HTTP/1.1
    ///   connections that each get their own congestion window and
    ///   saturate bandwidth in parallel.
    /// * **`network_concurrency` concurrent in-flight requests**,
    ///   matching pnpm's `networkConcurrency` default (see
    ///   [`default_network_concurrency`]). Pnpm uses a 50-socket
    ///   per-host pool ceiling (`DEFAULT_MAX_SOCKETS` in
    ///   `network/fetch/src/dispatcher.ts`) *and* a smaller
    ///   request-level cap that bounds how many fetches it actually
    ///   runs at once; pacquet's semaphore plays the second role.
    /// * **`User-Agent: pnpm`** matching pnpm's
    ///   `fetchFromRegistry.ts`. A default `reqwest::Client` sends
    ///   no UA, which can trip CDN / WAF rules that reject or RST
    ///   bot-shaped traffic before any HTTP response is produced.
    ///
    /// `pool_idle_timeout(4s)` matches
    /// [`agentkeepalive`'s](https://github.com/node-modules/agentkeepalive/blob/1e5e312f36/lib/agent.js#L39-L41)
    /// default `freeSocketTimeout` (the agent pnpm builds its
    /// connection pool on top of). Most CDN / load-balancer edges in
    /// front of `registry.npmjs.org` close idle sockets after 5–15s
    /// without sending FIN that hyper notices; a pool TTL above that
    /// lets pacquet reuse a half-dead socket and surface the next
    /// request as a generic "error sending request for url". 4s
    /// keeps the pool useful for back-to-back downloads (pacquet
    /// runs hundreds of fetches in seconds) but well below the
    /// typical edge keepalive.
    ///
    /// `timeout(5min)` is the per-request deadline, not the socket
    /// inactivity timeout. A default `reqwest::Client` has no
    /// deadlines at all, so a stalled upstream hangs the install
    /// indefinitely. 5 min is deliberately generous — npm tarballs
    /// are usually under 5 MB but can reach hundreds of MB on slow
    /// connections — and catches truly stuck sockets, not
    /// short-lived hiccups.
    ///
    /// `hickory_dns(true)` swaps reqwest's default resolver
    /// (tokio's `lookup_host`, which calls the platform's blocking
    /// `getaddrinfo` from a `spawn_blocking` thread) for the
    /// pure-Rust async resolver. The default resolver is correct
    /// but on macOS it routes every lookup through `mDNSResponder`,
    /// which spuriously returns `EAI_NONAME` ("nodename nor servname
    /// provided") for valid hostnames when many concurrent lookups
    /// pile up — e.g. the [`default_network_concurrency`] simultaneous
    /// tarball connections this client opens. pnpm doesn't hit it
    /// because Node's `dns.lookup`
    /// runs on libuv's 4-thread pool, naturally throttling concurrent
    /// `getaddrinfo` calls. `hickory-dns` queries DNS over UDP / TCP
    /// directly, bypassing `mDNSResponder` and the EAI_NONAME flake
    /// entirely.
    pub fn new_for_installs() -> Self {
        Self::for_installs(&ProxyConfig::default(), &TlsConfig::default())
            .expect("default ProxyConfig + TlsConfig carry no URLs/PEMs and cannot fail")
    }

    /// Construct the install client with proxy + TLS configuration
    /// applied.
    ///
    /// Ports pnpm v11's
    /// [`getDispatcher`](https://github.com/pnpm/pnpm/blob/94240bc046/network/fetch/src/dispatcher.ts#L23-L31)
    /// onto reqwest:
    /// * **Proxy routing.** HTTPS targets route through `https_proxy`,
    ///   HTTP targets through `http_proxy`, and [`ProxyConfig::no_proxy`]
    ///   short-circuits both via a per-URL custom-proxy closure.
    ///   Basic-auth user/password halves embedded in the proxy URL
    ///   are percent-decoded before being forwarded as the
    ///   `Proxy-Authorization` header — matching upstream's
    ///   [decode at dispatcher.ts:180-182](https://github.com/pnpm/pnpm/blob/94240bc046/network/fetch/src/dispatcher.ts#L180-L182).
    /// * **TLS.** Each PEM in [`TlsConfig::ca`] is added as a trusted
    ///   root via `reqwest::Certificate::from_pem`. When both
    ///   [`TlsConfig::cert`] and [`TlsConfig::key`] are set, they are
    ///   passed to `Identity::from_pkcs8_pem` and installed as a
    ///   client [`Identity`]. `strict_ssl` defaults to `true` and
    ///   disables both chain-of-trust and hostname verification when
    ///   `false` — same as Node's `rejectUnauthorized=false`
    ///   short-circuit that pnpm forwards through undici
    ///   ([`dispatcher.ts:191,197,241,295`](https://github.com/pnpm/pnpm/blob/94240bc046/network/fetch/src/dispatcher.ts#L191)).
    /// * **`local_address`.** Pinned via
    ///   `reqwest::ClientBuilder::local_address`.
    ///
    /// Returns [`ProxyError::InvalidProxy`] when either configured
    /// proxy URL fails to parse even after the auto-`http://` prefix
    /// retry (matching upstream's `ERR_PNPM_INVALID_PROXY`), or
    /// [`TlsError`] when any CA or client identity PEM is malformed.
    /// pnpm does not define `ERR_PNPM_INVALID_CA` / similar codes —
    /// see [`TlsError`] for why pacquet still surfaces the failure
    /// eagerly rather than at request time.
    pub fn for_installs(proxy: &ProxyConfig, tls: &TlsConfig) -> Result<Self, ForInstallsError> {
        let https = proxy.https_proxy.as_deref().map(parse_proxy_url).transpose()?;
        let http = proxy.http_proxy.as_deref().map(parse_proxy_url).transpose()?;
        let no_proxy = Arc::new(NoProxyMatcher::from(proxy.no_proxy.as_ref()));

        let mut builder = default_client_builder();
        if let Some(url) = https {
            builder = builder.proxy(build_scheme_proxy(url, "https", Arc::clone(&no_proxy)));
        }
        if let Some(url) = http {
            builder = builder.proxy(build_scheme_proxy(url, "http", Arc::clone(&no_proxy)));
        }
        builder = apply_tls(builder, tls)?;
        let client = builder.build().expect("build reqwest client with default timeouts and proxy");
        Ok(ThrottledClient::from_client(client))
    }

    /// Construct a throttled client wrapping a pre-built [`Client`].
    /// Useful for tests that want different timeout values than
    /// [`Self::new_for_installs`] sets — e.g. sub-second connect
    /// timeouts so firewalled / unreachable URLs fail within the
    /// test-suite budget instead of waiting on TCP retry.
    pub fn from_client(client: Client) -> Self {
        let semaphore = Semaphore::new(default_network_concurrency());
        ThrottledClient { semaphore, client }
    }
}

/// Shared builder with the install-time defaults
/// ([`ThrottledClient::new_for_installs`] documents the why behind each
/// setting). Both `new_for_installs` and [`ThrottledClient::for_installs`]
/// route through this helper so a single source of truth governs
/// timeouts, HTTP-version, resolver, and the default User-Agent header.
fn default_client_builder() -> reqwest::ClientBuilder {
    let mut default_headers = HeaderMap::with_capacity(1);
    default_headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
    Client::builder()
        .http1_only()
        .default_headers(default_headers)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .pool_idle_timeout(Duration::from_secs(4))
        .hickory_dns(true)
}

/// Apply [`TlsConfig`] onto a [`reqwest::ClientBuilder`]: register each
/// CA, install the client identity, set `danger_accept_invalid_certs`
/// when `strict_ssl: false`, and pin the outbound interface. Returns
/// the modified builder unchanged when every field is `None` / empty —
/// matching pnpm's "TLS-unset is default-TLS" semantics.
///
/// `strict_ssl` defaults to `true` here (`unwrap_or(true)`) rather than
/// in the config layer because that's where pnpm applies the same
/// default — see the "Defaults" section of [`TlsConfig`]. Failures from
/// PEM parsing surface as [`TlsError::InvalidCa`] /
/// [`TlsError::InvalidClientIdentity`] and bubble through
/// [`ForInstallsError`].
fn apply_tls(
    mut builder: reqwest::ClientBuilder,
    tls: &TlsConfig,
) -> Result<reqwest::ClientBuilder, TlsError> {
    for (index, pem) in tls.ca.iter().enumerate() {
        let cert = Certificate::from_pem(pem.as_bytes())
            .map_err(|source| TlsError::InvalidCa { index, reason: source.to_string() })?;
        builder = builder.add_root_certificate(cert);
    }
    if let (Some(cert), Some(key)) = (tls.cert.as_deref(), tls.key.as_deref()) {
        // reqwest's `Identity::from_pkcs8_pem` (gated on the
        // `native-tls` feature pacquet builds with) takes cert and
        // key as two separate PEM buffers — same shape pnpm hands to
        // undici. The key must be in PKCS#8 PEM format
        // (`-----BEGIN PRIVATE KEY-----`). Legacy PKCS#1 keys
        // (`-----BEGIN RSA PRIVATE KEY-----`) and the
        // `from_pkcs12_der` path are not supported by this constructor
        // and would surface as `InvalidClientIdentity` here. pnpm /
        // Node `tls` accept both formats; the native-tls backend
        // doesn't. If a user reports a PKCS#1 key being rejected,
        // either convert with `openssl pkcs8 -topk8 -nocrypt` or
        // upgrade pacquet's reqwest TLS backend.
        let identity = Identity::from_pkcs8_pem(cert.as_bytes(), key.as_bytes())
            .map_err(|source| TlsError::InvalidClientIdentity { reason: source.to_string() })?;
        builder = builder.identity(identity);
    }
    // pnpm's `strict-ssl` default is `true`, applied at every
    // dispatcher emit site rather than at parse time.
    if !tls.strict_ssl.unwrap_or(true) {
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Some(addr) = tls.local_address {
        builder = builder.local_address(addr);
    }
    Ok(builder)
}

/// Error surface of [`ThrottledClient::for_installs`]. Wraps either a
/// proxy URL failure or a TLS material failure — the caller gets one
/// error type to handle regardless of which side of `for_installs`
/// rejected the input.
#[derive(Debug, derive_more::Display, derive_more::Error, miette::Diagnostic)]
#[non_exhaustive]
pub enum ForInstallsError {
    #[diagnostic(transparent)]
    Proxy(#[error(source)] ProxyError),

    #[diagnostic(transparent)]
    Tls(#[error(source)] TlsError),
}

impl From<ProxyError> for ForInstallsError {
    fn from(value: ProxyError) -> Self {
        ForInstallsError::Proxy(value)
    }
}

impl From<TlsError> for ForInstallsError {
    fn from(value: TlsError) -> Self {
        ForInstallsError::Tls(value)
    }
}

/// Build a [`Proxy`] that routes only requests whose target scheme matches
/// `scheme` ("http" or "https") and whose host doesn't fall under the
/// no-proxy bypass. Userinfo is stripped from the URL and re-attached
/// via [`Proxy::basic_auth`] after percent-decoding so usernames /
/// passwords with `%XX` escapes (e.g. `@` in a password) reach the
/// upstream proxy decoded — matching pnpm's behavior at
/// [`dispatcher.ts:180-182`](https://github.com/pnpm/pnpm/blob/94240bc046/network/fetch/src/dispatcher.ts#L180-L182).
fn build_scheme_proxy(
    url: reqwest::Url,
    scheme: &'static str,
    no_proxy: Arc<NoProxyMatcher>,
) -> Proxy {
    let (clean_url, auth) = strip_userinfo(url);
    let mut proxy = Proxy::custom(move |target| {
        if no_proxy.matches_url(target) {
            return None;
        }
        (target.scheme() == scheme).then(|| clean_url.clone())
    });
    if let Some((user, pass)) = auth {
        proxy = proxy.basic_auth(&user, &pass);
    }
    proxy
}

/// Default number of concurrent in-flight network requests.
///
/// Mirrors pnpm's `networkConcurrency` formula in
/// [`installing/package-requester/src/packageRequester.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/installing/package-requester/src/packageRequester.ts#L97)
/// and `calcMaxWorkers` in
/// [`worker/src/index.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/worker/src/index.ts#L63-L72):
///
/// ```ts
/// networkConcurrency = Math.min(64, Math.max(calcMaxWorkers() * 3, 16))
/// // calcMaxWorkers() = Math.max(1, availableParallelism() - 1)
/// ```
///
/// Concretely: 16 on a 4-core machine, 21 on 8-core, 27 on 10-core,
/// 45 on 16-core, capped at 64.
///
/// Uses [`std::thread::available_parallelism`] rather than
/// `num_cpus::get()` so cgroup / CPU-quota limits in containers and
/// CI runners are respected — `num_cpus` reports the host's logical
/// CPU count, which on a quota-limited runner can over-report and
/// push effective concurrency past what the kernel will actually
/// schedule (matching the convention `crates/cli` already uses for
/// rayon pool sizing, see `crates/cli/src/lib.rs`).
pub fn default_network_concurrency() -> usize {
    let available_parallelism =
        std::thread::available_parallelism().map(NonZeroUsize::get).unwrap_or(1);
    let max_workers = available_parallelism.saturating_sub(1).max(1);
    max_workers.saturating_mul(3).clamp(16, 64)
}

/// This is only necessary for tests.
impl Default for ThrottledClient {
    fn default() -> Self {
        ThrottledClient::new_for_installs()
    }
}

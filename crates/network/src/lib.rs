use reqwest::{
    header::{HeaderMap, HeaderValue, USER_AGENT},
    Client,
};
use std::{future::IntoFuture, time::Duration};
use tokio::sync::Semaphore;

/// Default `User-Agent` pacquet sends on every registry request.
///
/// Identical to pnpm v11's
/// [`network/fetch/src/fetchFromRegistry.ts`](https://github.com/pnpm/pnpm/blob/main/network/fetch/src/fetchFromRegistry.ts#L9):
/// the literal string `pnpm`. A default `reqwest::Client` sends *no*
/// User-Agent at all, which some registry CDNs and corporate WAFs
/// treat as a bot signature and either block at the edge or terminate
/// mid-handshake (surfacing as a generic "error sending request for
/// url" with no body to look at).
///
/// We deliberately send `pnpm` rather than `pacquet/<version>` for
/// two reasons:
///
/// 1. **Pacquet is a port of pnpm** — its goal is byte-for-byte
///    behavioural parity, including what the registry sees on the
///    wire, so any UA-keyed allow / rate-limit rule that lets pnpm
///    through also lets pacquet through.
/// 2. The user reported a `pump-2.0.1.tgz` fetch that consistently
///    failed under pacquet but succeeded under a parallel `pnpm` on
///    the same network; reproducing pnpm's UA exactly is the most
///    direct fix that doesn't require speculating about which CDN
///    rule we're tripping.
const DEFAULT_USER_AGENT: &str = "pnpm";

/// Wrapper around [`Client`] with concurrent request limit enforced by the [`Semaphore`] mechanism.
#[derive(Debug)]
pub struct ThrottledClient {
    semaphore: Semaphore,
    client: Client,
}

impl ThrottledClient {
    /// Acquire a permit and run `proc` with the underlying [`Client`].
    pub async fn run_with_permit<Proc, ProcFuture>(&self, proc: Proc) -> ProcFuture::Output
    where
        Proc: FnOnce(&Client) -> ProcFuture,
        ProcFuture: IntoFuture,
    {
        let permit =
            self.semaphore.acquire().await.expect("semaphore shouldn't have been closed this soon");
        let result = proc(&self.client).await;
        drop(permit);
        result
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
    /// * **50 concurrent sockets**, matching pnpm's
    ///   `DEFAULT_MAX_SOCKETS`.
    /// * **`User-Agent: pnpm`** matching pnpm's
    ///   `fetchFromRegistry.ts`. A default `reqwest::Client` sends
    ///   no UA, which can trip CDN / WAF rules that reject or RST
    ///   bot-shaped traffic before any HTTP response is produced —
    ///   exactly the symptom reported when the headerless client
    ///   failed to fetch `pump-2.0.1.tgz` while a parallel `pnpm`
    ///   on the same network succeeded.
    ///
    ///   We deliberately do *not* set a default `Accept` header —
    ///   pnpm's `fetchFromRegistry` always attaches
    ///   `application/vnd.npm.install-v1+json; …` to every request,
    ///   including tarball fetches where it makes no sense, but
    ///   that's an upstream quirk we have no reason to copy.
    ///   `crates/registry`'s metadata calls set the npm-specific
    ///   `Accept` per-request; tarball fetches send no `Accept` and
    ///   the registry serves them just fine.
    ///
    /// `pool_idle_timeout(4s)` matches
    /// [`agentkeepalive`'s](https://github.com/node-modules/agentkeepalive/blob/master/lib/agent.js#L39-L41)
    /// default `freeSocketTimeout` (the agent pnpm builds its
    /// connection pool on top of). Most CDN / load-balancer edges in
    /// front of `registry.npmjs.org` close idle sockets after 5–15s
    /// without sending FIN that hyper notices, so a longer pool TTL
    /// (the previous 30s) lets pacquet reuse a half-dead socket and
    /// surface the next request as a generic "error sending request
    /// for url" — same symptom the user hit on `pump-2.0.1.tgz` while
    /// pnpm on the same network succeeded. 4s keeps the pool useful
    /// for back-to-back downloads (pacquet runs hundreds of fetches
    /// in seconds) but well below the typical edge keepalive.
    ///
    /// `timeout(5min)` is the per-request deadline, not the socket
    /// inactivity timeout. A default `reqwest::Client` has no
    /// deadlines at all, which is how `integrated-benchmark` used to
    /// hang at "Benchmark 1: pacquet@HEAD" until the GHA step budget
    /// (#263) when an upstream stalled. 5 min is deliberately
    /// generous — npm tarballs are usually under 5 MB but can reach
    /// hundreds of MB on slow connections. The retry loop in
    /// `crates/tarball` (#301) handles short, transient failures;
    /// the 5-minute cap catches truly stuck sockets. Making these
    /// values user-configurable (npmrc / env / CLI) is follow-up.
    ///
    /// `hickory_dns(true)` swaps reqwest's default resolver
    /// (tokio's `lookup_host`, which calls the platform's blocking
    /// `getaddrinfo` from a `spawn_blocking` thread) for the
    /// pure-Rust async resolver. The default resolver is correct
    /// but on macOS it routes every lookup through `mDNSResponder`,
    /// which spuriously returns `EAI_NONAME` ("nodename nor servname
    /// provided") for valid hostnames when many concurrent lookups
    /// pile up — e.g. the 50 simultaneous tarball connections this
    /// client opens. pnpm doesn't hit it because Node's `dns.lookup`
    /// runs on libuv's 4-thread pool, naturally throttling concurrent
    /// `getaddrinfo` calls. `hickory-dns` queries DNS over UDP / TCP
    /// directly, bypassing `mDNSResponder` and the EAI_NONAME flake
    /// entirely.
    pub fn new_for_installs() -> Self {
        let mut default_headers = HeaderMap::with_capacity(1);
        default_headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));

        let client = Client::builder()
            .http1_only()
            .default_headers(default_headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .pool_idle_timeout(Duration::from_secs(4))
            .hickory_dns(true)
            .build()
            .expect("build reqwest client with default timeouts");
        ThrottledClient::from_client(client)
    }

    /// Construct a throttled client wrapping a pre-built [`Client`].
    /// Useful for tests that want different timeout values than
    /// [`Self::new_for_installs`] sets — e.g. sub-second connect
    /// timeouts so firewalled / unreachable URLs fail within the
    /// test-suite budget instead of waiting on TCP retry.
    pub fn from_client(client: Client) -> Self {
        // Matches pnpm v11's `DEFAULT_MAX_SOCKETS`
        // (`network/fetch/src/dispatcher.ts:12`). Pnpm has explicit
        // benchmark evidence that 50 HTTP/1.1 connections saturate
        // tarball-fetch bandwidth better than fewer-with-multiplexing
        // or fewer-connections-period.
        const MAX_CONCURRENT_REQUESTS: usize = 50;
        let semaphore = Semaphore::new(MAX_CONCURRENT_REQUESTS);
        ThrottledClient { semaphore, client }
    }
}

/// This is only necessary for tests.
impl Default for ThrottledClient {
    fn default() -> Self {
        ThrottledClient::new_for_installs()
    }
}

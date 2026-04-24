use reqwest::Client;
use std::{future::IntoFuture, time::Duration};
use tokio::sync::Semaphore;

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
    ///   `DEFAULT_MAX_SOCKETS`. The old `num_cpus.max(16)` semaphore
    ///   under-subscribed on every machine we benchmarked — on a
    ///   4-core GHA runner pacquet had 1/3 of pnpm's concurrent-
    ///   fetch budget.
    ///
    /// Timeouts are unchanged: a default `reqwest::Client` has no
    /// deadlines at all, which is how `integrated-benchmark` used to
    /// hang at "Benchmark 1: pacquet@HEAD" until the GHA step budget
    /// (#263) when an upstream stalled. The 5-minute `timeout` is
    /// deliberately generous — npm tarballs are usually under 5 MB
    /// but can reach hundreds of MB on slow connections, and there's
    /// no retry on transient network errors yet (#259). 5 min keeps
    /// slow-but-progressing downloads succeeding while still catching
    /// truly stuck sockets. Making these values user-configurable
    /// (npmrc / env / CLI) is follow-up once the fetch-retry story
    /// lands.
    pub fn new_for_installs() -> Self {
        let client = Client::builder()
            .http1_only()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .pool_idle_timeout(Duration::from_secs(30))
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

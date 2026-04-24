use pipe_trait::Pipe;
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

    /// Construct a new throttled client based on the number of CPUs.
    /// If the number of CPUs is greater than 16, the number of permits will be equal to the number of CPUs.
    /// Otherwise, the number of permits will be 16.
    ///
    /// The returned [`Client`] carries explicit `connect` / `request` /
    /// `pool_idle` deadlines. A default `reqwest::Client` has none of
    /// these, and the CLI uses this constructor for real registry
    /// traffic as well as the bench's local verdaccio — without a
    /// request deadline pacquet just sits on a half-open socket
    /// forever when an upstream stalls (GC pause, uplink stall, TCP
    /// packet loss without RST). That's how `integrated-benchmark`
    /// ends up hanging at "Benchmark 1: pacquet@HEAD" until the GHA
    /// step timeout, see #263.
    ///
    /// The 5-minute `timeout` is deliberately generous: npm tarballs
    /// are usually under 5 MB but can reach tens or even hundreds of
    /// MB on slow connections, and there's no retry on transient
    /// network errors yet (#259). 5 min keeps slow-but-progressing
    /// downloads succeeding while still catching truly stuck sockets
    /// inside the bench's step budget. Making these values
    /// user-configurable (npmrc / env / CLI) is the natural next step
    /// once the fetch-retry story is in place — left as follow-up so
    /// this stays a minimal, PR-reviewable fix for the CI hang.
    pub fn new_from_cpu_count() -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .expect("build reqwest client with default timeouts");
        ThrottledClient::from_client(client)
    }

    /// Construct a throttled client wrapping a pre-built [`Client`].
    /// Useful for tests that want different timeout values than
    /// [`Self::new_from_cpu_count`] sets — e.g. sub-second connect
    /// timeouts so firewalled / unreachable URLs fail within the
    /// test-suite budget instead of waiting on TCP retry.
    pub fn from_client(client: Client) -> Self {
        const MIN_PERMITS: usize = 16;
        let semaphore = num_cpus::get().max(MIN_PERMITS).pipe(Semaphore::new);
        ThrottledClient { semaphore, client }
    }
}

/// This is only necessary for tests.
impl Default for ThrottledClient {
    fn default() -> Self {
        ThrottledClient::new_from_cpu_count()
    }
}

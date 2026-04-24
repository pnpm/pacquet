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
    /// The returned [`Client`] has explicit `connect` / `request` /
    /// `pool_idle` timeouts set. A default `reqwest::Client` has none of
    /// these, and on CI the bench's single-process verdaccio occasionally
    /// stops responding (GC pause, uplink stall, TCP packet loss without
    /// RST) — without a request deadline pacquet just sits on the
    /// half-open socket forever, which is how `integrated-benchmark`
    /// ends up hanging at "Benchmark 1: pacquet@HEAD" until the GHA step
    /// timeout (see #263). 60 s per request is generous for localhost
    /// tarballs (typical npm tarball is <5 MB) but short enough that a
    /// real hang fails fast and hyperfine surfaces the error.
    pub fn new_from_cpu_count() -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
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

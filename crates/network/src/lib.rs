use pipe_trait::Pipe;
use reqwest::Client;
use std::future::IntoFuture;
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
    pub fn new_from_cpu_count() -> Self {
        const MIN_PERMITS: usize = 16;
        let semaphore = num_cpus::get().max(MIN_PERMITS).pipe(Semaphore::new);
        let client = Client::new();
        ThrottledClient { semaphore, client }
    }
}

/// This is only necessary for tests.
impl Default for ThrottledClient {
    fn default() -> Self {
        ThrottledClient::new_from_cpu_count()
    }
}

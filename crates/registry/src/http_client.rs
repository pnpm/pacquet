use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};

use crate::{error::RegistryError, package::Package};

pub struct HttpClient {
    client: ClientWithMiddleware,
    cache: elsa::FrozenMap<String, Box<Package>>,
}

impl HttpClient {
    pub fn new() -> Self {
        let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);
        let client = ClientBuilder::new(reqwest::Client::new())
            .with(RetryTransientMiddleware::new_with_policy(retry_policy))
            .build();

        HttpClient { client, cache: elsa::FrozenMap::new() }
    }

    pub async fn get_package(&self, name: &str) -> Result<&Package, RegistryError> {
        if let Some(package) = &self.cache.get(name) {
            return Ok(package);
        }

        let package: Package = self
            .client
            .get(format!("https://registry.npmjs.com/{name}"))
            .header("user-agent", "pacquet-cli")
            .header("content-type", "application/json")
            .send()
            .await?
            .json::<Package>()
            .await?;

        let package = self.cache.insert(name.to_string(), Box::new(package));

        Ok(package)
    }
}

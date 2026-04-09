use anyhow::{Context, Result, anyhow};
use reqwest::{Client, ClientBuilder};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, warn};

#[derive(Clone)]
pub struct BunnyCdn {
    client: Client,
    api_key: Arc<str>,
}

impl BunnyCdn {
    pub fn new(api_key: String) -> Self {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(5)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            api_key: Arc::from(api_key.into_boxed_str()),
        }
    }

    pub async fn purge_url(&self, url: &str) -> Result<()> {
        let encoded_url = urlencoding::encode(url);
        let purge_url = format!("https://api.bunny.net/purge?url={}", encoded_url);

        debug!(event = "cdn.purge.request", url = %url);

        let response = self
            .client
            .post(&purge_url)
            .header("AccessKey", self.api_key.as_ref())
            .send()
            .await
            .context("failed to purge URL")?;

        if response.status().is_success() {
            debug!(event = "cdn.purge.success", url = %url);
            Ok(())
        } else {
            Err(anyhow!("failed to purge URL: {}", response.status()))
        }
    }

    pub async fn purge_urls_parallel(
        &self,
        urls: &[String],
        concurrency: usize,
    ) -> Vec<(String, Result<()>)> {
        if urls.is_empty() {
            return Vec::new();
        }

        let semaphore = Arc::new(Semaphore::new(concurrency));
        let mut tasks = JoinSet::new();

        for url in urls.iter().cloned() {
            let cdn = self.clone();
            let permit = Arc::clone(&semaphore);

            tasks.spawn(async move {
                let _permit = permit.acquire().await;
                (url.clone(), cdn.purge_url(&url).await)
            });
        }

        let mut results = Vec::with_capacity(urls.len());
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((url, res)) => results.push((url, res)),
                Err(e) => {
                    warn!(event = "cdn.purge.task_failed", error = %e);
                }
            }
        }

        results
    }
}

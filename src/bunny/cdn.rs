use anyhow::{anyhow, Context, Result};
use reqwest::Client;

#[derive(Clone)]
pub struct BunnyCdn {
    client: Client,
    api_key: String,
}

impl BunnyCdn {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }

    pub async fn purge_url(&self, url: &str) -> Result<()> {
        let encoded_url = urlencoding::encode(url);
        let purge_url = format!("https://api.bunny.net/purge?url={}", encoded_url);

        let response = self
            .client
            .post(&purge_url)
            .header("AccessKey", &self.api_key)
            .send()
            .await
            .context("failed to purge URL")?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!(
                "failed to purge URL: {}",
                response.status()
            ))
        }
    }

    #[allow(dead_code)]
    pub async fn purge_urls(&self, urls: &[String]) -> Vec<(String, Result<()>)> {
        let mut results = Vec::new();
        for url in urls {
            let result = self.purge_url(url).await;
            results.push((url.clone(), result));
        }
        results
    }
}

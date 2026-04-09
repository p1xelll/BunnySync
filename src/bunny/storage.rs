use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;

pub struct BunnyStorage {
    client: Client,
    storage_zone: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct FileInfo {
    #[serde(rename = "ObjectName")]
    pub name: String,
    #[serde(rename = "Checksum")]
    pub checksum: Option<String>,
}

impl Clone for BunnyStorage {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            storage_zone: self.storage_zone.clone(),
            password: self.password.clone(),
        }
    }
}

impl BunnyStorage {
    pub fn new(storage_zone: String, password: String) -> Self {
        Self {
            client: Client::new(),
            storage_zone,
            password,
        }
    }

    pub async fn list_files(&self, path: &str) -> Result<HashMap<String, String>> {
        let url = format!(
            "https://storage.bunnycdn.com/{}/{}",
            self.storage_zone,
            path.trim_start_matches('/')
        );

        let response = self
            .client
            .get(&url)
            .header("AccessKey", &self.password)
            .send()
            .await
            .context("failed to list files")?;

        if response.status().is_success() {
            let files: Vec<FileInfo> =
                response.json().await.context("failed to parse file list")?;
            let mut map = HashMap::new();
            for file in files {
                if let Some(checksum) = file.checksum {
                    map.insert(file.name, checksum);
                }
            }
            Ok(map)
        } else if response.status().as_u16() == 404 {
            Ok(HashMap::new())
        } else {
            Err(anyhow!("failed to list files: {}", response.status()))
        }
    }

    pub async fn upload_file(&self, remote_path: &str, content: &[u8]) -> Result<()> {
        let encoded_path = urlencoding::encode(remote_path);
        let url = format!(
            "https://storage.bunnycdn.com/{}/{}",
            self.storage_zone, encoded_path
        );

        let response = self
            .client
            .put(&url)
            .header("AccessKey", &self.password)
            .body(content.to_vec())
            .send()
            .await
            .context("failed to upload file")?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("failed to upload file: {}", response.status()))
        }
    }

    pub async fn delete_file(&self, remote_path: &str) -> Result<()> {
        let encoded_path = urlencoding::encode(remote_path);
        let url = format!(
            "https://storage.bunnycdn.com/{}/{}",
            self.storage_zone, encoded_path
        );

        let response = self
            .client
            .delete(&url)
            .header("AccessKey", &self.password)
            .send()
            .await
            .context("failed to delete file")?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("failed to delete file: {}", response.status()))
        }
    }
}

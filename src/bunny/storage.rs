use crate::types::RemoteFileSet;
use anyhow::{Context, Result, anyhow};
use reqwest::{Client, ClientBuilder};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tracing::debug;

pub struct BunnyStorage {
    client: Client,
    storage_zone: Arc<str>,
    password: Arc<str>,
}

#[derive(Debug, Deserialize)]
pub struct FileInfo {
    #[serde(rename = "ObjectName")]
    pub name: String,
    #[serde(rename = "Checksum")]
    pub checksum: Option<String>,
    #[serde(rename = "IsDirectory")]
    pub is_directory: Option<bool>,
}

impl Clone for BunnyStorage {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            storage_zone: Arc::clone(&self.storage_zone),
            password: Arc::clone(&self.password),
        }
    }
}

fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        let mut result = String::with_capacity(dir.len() + name.len() + 1);
        result.push_str(dir);
        result.push('/');
        result.push_str(name);
        result
    }
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(urlencoding::encode)
        .collect::<Vec<_>>()
        .join("/")
}

impl BunnyStorage {
    pub fn new(storage_zone: String, password: String) -> Self {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .http2_prior_knowledge()
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            storage_zone: Arc::from(storage_zone.into_boxed_str()),
            password: Arc::from(password.into_boxed_str()),
        }
    }

    pub async fn list_files(&self, _path: &str) -> Result<RemoteFileSet> {
        let mut all_files = HashMap::new();
        let mut all_directories = Vec::new();
        let mut dirs_to_scan = vec![String::new()];

        while let Some(dir) = dirs_to_scan.pop() {
            let url = if dir.is_empty() {
                format!("https://storage.bunnycdn.com/{}/", self.storage_zone)
            } else {
                format!(
                    "https://storage.bunnycdn.com/{}/{}/",
                    self.storage_zone, dir
                )
            };

            debug!(event = "storage.list_files.request", url = %url, dir = %dir);

            let response = self
                .client
                .get(&url)
                .header("AccessKey", self.password.as_ref())
                .send()
                .await
                .context("failed to list files")?;

            if response.status().as_u16() == 404 {
                continue;
            }

            if !response.status().is_success() {
                return Err(anyhow!("failed to list files: {}", response.status()));
            }

            let items: Vec<FileInfo> =
                response.json().await.context("failed to parse file list")?;

            debug!(
                event = "storage.list_files.parsed",
                dir = %dir,
                count = items.len(),
            );

            for item in items {
                let is_dir = item.is_directory.unwrap_or(false) || item.checksum.is_none();

                if is_dir {
                    let dir_path = join_path(&dir, &item.name);
                    if !dir_path.is_empty() {
                        all_directories.push(dir_path.clone());
                    }
                    dirs_to_scan.push(dir_path);
                } else if let Some(checksum) = item.checksum {
                    all_files.insert(join_path(&dir, &item.name), checksum);
                }
            }
        }

        debug!(
            event = "storage.list_files.complete",
            total_files = all_files.len(),
            total_dirs = all_directories.len(),
        );
        Ok(RemoteFileSet {
            files: all_files,
            directories: all_directories,
        })
    }

    pub async fn upload_file_from_path(
        &self,
        remote_path: &str,
        file_path: &std::path::Path,
    ) -> Result<()> {
        let url = format!(
            "https://storage.bunnycdn.com/{}/{}",
            self.storage_zone,
            encode_path(remote_path)
        );

        let mut file = tokio::fs::File::open(file_path)
            .await
            .context("failed to open file")?;

        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut content = Vec::new();

        loop {
            let n = file
                .read(&mut buffer)
                .await
                .context("failed to read file")?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            content.extend_from_slice(&buffer[..n]);
        }

        let checksum_hex = hex::encode_upper(hasher.finalize());

        let response = self
            .client
            .put(&url)
            .header("AccessKey", self.password.as_ref())
            .header("Checksum", &checksum_hex)
            .body(content)
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
        let url = format!(
            "https://storage.bunnycdn.com/{}/{}",
            self.storage_zone,
            encode_path(remote_path)
        );

        let response = self
            .client
            .delete(&url)
            .header("AccessKey", self.password.as_ref())
            .send()
            .await
            .context("failed to delete file")?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("failed to delete file: {}", response.status()))
        }
    }

    pub async fn delete_directory(&self, remote_path: &str) -> Result<()> {
        let url = format!(
            "https://storage.bunnycdn.com/{}/{}/",
            self.storage_zone,
            encode_path(remote_path)
        );

        debug!(
            event = "storage.delete_directory.request",
            path = %remote_path,
            url = %url
        );

        let response = self
            .client
            .delete(&url)
            .header("AccessKey", self.password.as_ref())
            .send()
            .await
            .context("failed to delete directory")?;

        if response.status().is_success() || response.status().as_u16() == 404 {
            debug!(
                event = "storage.delete_directory.success",
                path = %remote_path,
                status = %response.status()
            );
            Ok(())
        } else {
            Err(anyhow!("failed to delete directory: {}", response.status()))
        }
    }
}

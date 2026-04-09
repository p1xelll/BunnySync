use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tracing::debug;

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
    #[serde(rename = "IsDirectory")]
    pub is_directory: Option<bool>,
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

fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}" , dir, name)
    }
}

fn encode_path(path: &str) -> String {
    path.split('/').map(urlencoding::encode).collect::<Vec<_>>().join("/")
}

impl BunnyStorage {
    pub fn new(storage_zone: String, password: String) -> Self {
        Self {
            client: Client::new(),
            storage_zone,
            password,
        }
    }

    /// Recursively list all files in storage zone
    /// Bunny Storage Edge requires manual recursion for subdirectories
    pub async fn list_files(&self, _path: &str) -> Result<HashMap<String, String>> {
        let mut all_files = HashMap::new();
        let mut dirs_to_scan = vec!["".to_string()];

        while let Some(dir) = dirs_to_scan.pop() {
            // Bunny Storage API requires trailing slash for directory listing
            let url = if dir.is_empty() {
                format!("https://storage.bunnycdn.com/{}/", self.storage_zone)
            } else {
                format!("https://storage.bunnycdn.com/{}/{}/", self.storage_zone, dir)
            };

            debug!(event = "storage.list_files.request", url = %url, dir = %dir);

            let response = self
                .client
                .get(&url)
                .header("AccessKey", &self.password)
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
                    dirs_to_scan.push(join_path(&dir, &item.name));
                } else if let Some(checksum) = item.checksum {
                    all_files.insert(join_path(&dir, &item.name), checksum);
                }
            }
        }

        debug!(
            event = "storage.list_files.complete",
            total_files = all_files.len(),
        );
        Ok(all_files)
    }

    pub async fn upload_file(&self, remote_path: &str, content: &[u8]) -> Result<()> {
        let url = format!(
            "https://storage.bunnycdn.com/{}/{}",
            self.storage_zone,
            encode_path(remote_path)
        );

        // Calculate SHA-256 checksum
        let checksum = Sha256::digest(content);
        let checksum_hex = hex::encode_upper(checksum);

        let response = self
            .client
            .put(&url)
            .header("AccessKey", &self.password)
            .header("Checksum", &checksum_hex)
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
        let url = format!(
            "https://storage.bunnycdn.com/{}/{}",
            self.storage_zone,
            encode_path(remote_path)
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

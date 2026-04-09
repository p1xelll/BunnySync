use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum FileAction {
    Skip,
    Upload,
    Delete,
}

#[derive(Debug, Clone)]
pub struct FileDelta {
    pub path: String,
    pub action: FileAction,
    #[allow(dead_code)]
    pub local_checksum: Option<String>,
    pub remote_checksum: Option<String>,
}

pub fn compute_delta(
    local_files: &HashMap<String, String>,
    remote_files: &HashMap<String, String>,
) -> Vec<FileDelta> {
    let mut deltas = Vec::new();

    for (path, local_checksum) in local_files {
        match remote_files.get(path) {
            None => {
                deltas.push(FileDelta {
                    path: path.clone(),
                    action: FileAction::Upload,
                    local_checksum: Some(local_checksum.clone()),
                    remote_checksum: None,
                });
            }
            Some(remote_checksum) => {
                if local_checksum == remote_checksum {
                    deltas.push(FileDelta {
                        path: path.clone(),
                        action: FileAction::Skip,
                        local_checksum: Some(local_checksum.clone()),
                        remote_checksum: Some(remote_checksum.clone()),
                    });
                } else {
                    deltas.push(FileDelta {
                        path: path.clone(),
                        action: FileAction::Upload,
                        local_checksum: Some(local_checksum.clone()),
                        remote_checksum: Some(remote_checksum.clone()),
                    });
                }
            }
        }
    }

    for (path, remote_checksum) in remote_files {
        if !local_files.contains_key(path) {
            deltas.push(FileDelta {
                path: path.clone(),
                action: FileAction::Delete,
                local_checksum: None,
                remote_checksum: Some(remote_checksum.clone()),
            });
        }
    }

    deltas
}

pub fn get_purge_urls(deltas: &[FileDelta], pull_zone_domain: &str) -> Vec<String> {
    deltas
        .iter()
        .filter(|d| {
            matches!(d.action, FileAction::Upload | FileAction::Delete)
                && d.remote_checksum.is_some()
        })
        .map(|d| format!("https://{}/{}", pull_zone_domain, d.path))
        .collect()
}

pub fn get_uploads(deltas: &[FileDelta]) -> Vec<&FileDelta> {
    deltas
        .iter()
        .filter(|d| matches!(d.action, FileAction::Upload))
        .collect()
}

pub fn get_deletions(deltas: &[FileDelta]) -> Vec<&FileDelta> {
    deltas
        .iter()
        .filter(|d| matches!(d.action, FileAction::Delete))
        .collect()
}

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
        let remote_checksum = remote_files.get(path);
        let action = match remote_checksum {
            None => FileAction::Upload,
            Some(rc) if local_checksum == rc => FileAction::Skip,
            Some(_) => FileAction::Upload,
        };
        deltas.push(FileDelta {
            path: path.clone(),
            action,
            local_checksum: Some(local_checksum.clone()),
            remote_checksum: remote_checksum.cloned(),
        });
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
    // Strip protocol if present (handle both "example.com" and "https://example.com")
    let domain = pull_zone_domain
        .strip_prefix("https://")
        .or_else(|| pull_zone_domain.strip_prefix("http://"))
        .unwrap_or(pull_zone_domain);

    deltas
        .iter()
        .filter(|d| {
            matches!(d.action, FileAction::Upload | FileAction::Delete)
                && d.remote_checksum.is_some()
        })
        .map(|d| format!("https://{}/{}", domain, d.path))
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

pub fn get_skips(deltas: &[FileDelta]) -> Vec<&FileDelta> {
    deltas
        .iter()
        .filter(|d| matches!(d.action, FileAction::Skip))
        .collect()
}

pub fn count_modified(deltas: &[FileDelta]) -> usize {
    deltas
        .iter()
        .filter(|d| matches!(d.action, FileAction::Upload) && d.remote_checksum.is_some())
        .count()
}

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileAction {
    Skip,
    Upload,
    Delete,
    DeleteDir,
}

#[derive(Debug, Clone)]
pub struct FileDelta {
    pub path: String,
    pub action: FileAction,
    pub remote_checksum: Option<String>,
}

pub fn compute_delta(
    local_files: &HashMap<String, String>,
    remote_files: &HashMap<String, String>,
    local_dirs: &[String],
    remote_dirs: &[String],
) -> Vec<FileDelta> {
    let mut deltas = Vec::with_capacity(local_files.len() + remote_files.len());

    // Process files - use iterator chain for better cache locality
    deltas.extend(local_files.iter().map(|(path, local_checksum)| {
        let remote_checksum = remote_files.get(path);
        let action = match remote_checksum {
            None => FileAction::Upload,
            Some(rc) if local_checksum == rc => FileAction::Skip,
            Some(_) => FileAction::Upload,
        };
        FileDelta {
            path: path.clone(),
            action,
            remote_checksum: remote_checksum.cloned(),
        }
    }));

    // Process deletions - only remote files not present locally
    deltas.extend(remote_files.iter().filter_map(|(path, remote_checksum)| {
        if local_files.contains_key(path) {
            None
        } else {
            Some(FileDelta {
                path: path.clone(),
                action: FileAction::Delete,
                remote_checksum: Some(remote_checksum.clone()),
            })
        }
    }));

    // Process directory deletions - only directories that exist remotely but not locally
    if !remote_dirs.is_empty() {
        let local_dirs_set: HashSet<&str> = local_dirs.iter().map(String::as_str).collect();

        deltas.extend(remote_dirs.iter().filter_map(|dir_path| {
            if local_dirs_set.contains(dir_path.as_str()) {
                return None;
            }

            // Check if any file in this directory is being uploaded
            let dir_prefix = if dir_path.ends_with('/') {
                dir_path.clone()
            } else {
                format!("{}/", dir_path)
            };

            let has_uploads = local_files.keys().any(|f| f.starts_with(&dir_prefix));
            if has_uploads {
                return None;
            }

            Some(FileDelta {
                path: dir_path.clone(),
                action: FileAction::DeleteDir,
                remote_checksum: None,
            })
        }));
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
        .map(|d| {
            let mut url = String::with_capacity(domain.len() + d.path.len() + 9);
            url.push_str("https://");
            url.push_str(domain);
            url.push('/');
            url.push_str(&d.path);
            url
        })
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

pub fn get_dir_deletions(deltas: &[FileDelta]) -> Vec<&FileDelta> {
    deltas
        .iter()
        .filter(|d| matches!(d.action, FileAction::DeleteDir))
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

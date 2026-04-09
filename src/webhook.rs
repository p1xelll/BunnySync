use crate::bunny::cdn::BunnyCdn;
use crate::bunny::storage::BunnyStorage;
use crate::config::ProjectConfig;
use crate::deploy_queue::DeployQueue;
use crate::diff::{
    compute_delta, count_modified, get_deletions, get_dir_deletions, get_purge_urls, get_skips,
    get_uploads,
};
use crate::providers::detect_provider;
use crate::signature_cache::SignatureCache;
use crate::types::LocalFileSet;
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use git2::{FetchOptions, Repository};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};
use walkdir::WalkDir;

const UPLOAD_CONCURRENCY: usize = 10;
const DELETE_CONCURRENCY: usize = 10;
const PURGE_CONCURRENCY: usize = 5;
const MAX_UPLOAD_RETRIES: u32 = 3;
const RETRY_BASE_DELAY_MS: u64 = 100;
const BUFFER_SIZE: usize = 64 * 1024; // 64KB buffer

#[derive(Serialize)]
struct DeployResponse {
    status: String,
    uploaded: usize,
    deleted: usize,
    modified: usize,
    purged: usize,
    skipped: usize,
    #[serde(rename = "dirs_deleted")]
    dirs_deleted: usize,
}

impl DeployResponse {
    fn success(
        uploaded: usize,
        deleted: usize,
        modified: usize,
        purged: usize,
        skipped: usize,
        dirs_deleted: usize,
    ) -> Self {
        Self {
            status: "deployed".to_string(),
            uploaded,
            deleted,
            modified,
            purged,
            skipped,
            dirs_deleted,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<crate::config::Config>,
    pub signature_cache: SignatureCache,
    pub deploy_queue: DeployQueue,
}

pub fn create_router(config: Arc<crate::config::Config>) -> Router {
    let state = AppState {
        config,
        signature_cache: SignatureCache::new(Duration::from_secs(300)),
        deploy_queue: DeployQueue::new(),
    };

    Router::new()
        .route("/health", get(health_handler))
        .route("/hook/{project_id}", post(handle_webhook))
        .with_state(state)
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "healthy")
}

async fn handle_webhook(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    info!(
        event = "webhook.received",
        project_id = %project_id,
        "webhook received"
    );

    let project = match state.config.projects.get(&project_id) {
        Some(p) => p,
        None => {
            warn!(
                event = "webhook.project_not_found",
                project_id = %project_id,
                "project not found"
            );
            return (StatusCode::NOT_FOUND, "project not found").into_response();
        }
    };

    let provider = match detect_provider(&headers) {
        Some(p) => p,
        None => {
            warn!(event = "webhook.unknown_provider", "unknown provider");
            return (StatusCode::BAD_REQUEST, "unknown provider").into_response();
        }
    };

    let signature = match provider.verify_signature(&body, &headers, &project.webhook_secret) {
        Ok(sig) => sig,
        Err(e) => {
            warn!(
                event = "webhook.signature_verification_failed",
                error = %e,
                "signature verification failed"
            );
            return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
        }
    };

    if state.signature_cache.contains(&signature).await {
        warn!(
            event = "webhook.replay_detected",
            project_id = %project_id,
            "replay attack detected - signature already used"
        );
        return (StatusCode::CONFLICT, "duplicate webhook").into_response();
    }

    state.signature_cache.insert(signature).await;

    let push_event = match provider.parse_push_event(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                event = "webhook.parse_failed",
                error = %e,
                "failed to parse push event"
            );
            return (StatusCode::BAD_REQUEST, "invalid payload").into_response();
        }
    };

    // Check if this is a test webhook (before == after)
    if push_event.is_test {
        info!(
            event = "webhook.test",
            project_id = %project_id,
            "test webhook detected - no deploy performed"
        );
        return (
            StatusCode::OK,
            "test webhook received - signature valid, no deploy",
        )
            .into_response();
    }

    info!(
        event = "deploy.started",
        project_id = %project_id,
        ref_name = %push_event.ref_name,
        commit = %push_event.commit,
        "starting deployment"
    );

    let _deploy_permit: OwnedSemaphorePermit = match state.deploy_queue.acquire(&project_id).await {
        Some(permit) => permit,
        None => {
            warn!(
                event = "deploy.already_in_progress",
                project_id = %project_id,
                "deploy already in progress for this project"
            );
            return (StatusCode::CONFLICT, "deploy already in progress").into_response();
        }
    };

    let api_key = project
        .bunny_api_key
        .clone()
        .unwrap_or_else(|| state.config.bunny_api_key.clone());

    match deploy_project(project, &push_event, api_key).await {
        Ok(stats) => {
            info!(
                event = "deploy.completed",
                project_id = %project_id,
                uploaded = stats.uploaded,
                deleted = stats.deleted,
                modified = stats.modified,
                purged = stats.purged,
                skipped = stats.skipped,
                dirs_deleted = stats.dirs_deleted,
                "deploy completed successfully"
            );
            let response = DeployResponse::success(
                stats.uploaded,
                stats.deleted,
                stats.modified,
                stats.purged,
                stats.skipped,
                stats.dirs_deleted,
            );
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => {
            error!(
                event = "deploy.failed",
                project_id = %project_id,
                error = %e,
                "deploy failed"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "deploy failed").into_response()
        }
    }
}

#[derive(Debug)]
struct DeployStats {
    uploaded: usize,
    deleted: usize,
    modified: usize,
    purged: usize,
    skipped: usize,
    dirs_deleted: usize,
}

async fn deploy_project(
    project: &ProjectConfig,
    push_event: &crate::providers::PushEvent,
    api_key: String,
) -> Result<DeployStats> {
    let temp_dir = TempDir::new().context("failed to create temp directory")?;
    let repo_path = temp_dir.path().join("repo");

    let branch = push_event
        .ref_name
        .strip_prefix("refs/heads/")
        .unwrap_or(&push_event.ref_name);
    clone_repo(&project.repo_url, &repo_path, branch).await?;

    let local_files = collect_local_files(&repo_path).await?;

    let storage = BunnyStorage::new(
        project.bunny_storage_zone.clone(),
        project.bunny_storage_password.clone(),
    );

    let remote_files = storage.list_files("").await?;

    debug!(
        event = "deploy.file_counts",
        local_count = local_files.files.len(),
        remote_count = remote_files.files.len(),
        local_dirs = local_files.directories.len(),
        remote_dirs = remote_files.directories.len(),
        "local vs remote file counts"
    );

    let deltas = compute_delta(
        &local_files.files,
        &remote_files.files,
        &local_files.directories,
        &remote_files.directories,
    );

    // Calculate stats before consuming the vectors
    let uploaded_count = get_uploads(&deltas).len();
    let deleted_count = get_deletions(&deltas).len();
    let modified_count = count_modified(&deltas);
    let skipped_count = get_skips(&deltas).len();
    let dir_deletions = get_dir_deletions(&deltas);
    let dir_deletion_count = dir_deletions.len();

    info!(
        event = "delta.computed",
        local_files = local_files.files.len(),
        remote_files = remote_files.files.len(),
        local_dirs = local_files.directories.len(),
        remote_dirs = remote_files.directories.len(),
        uploads = uploaded_count,
        deletions = deleted_count,
        dir_deletions = dir_deletion_count,
        modified = modified_count,
        skipped = skipped_count,
        "delta computed"
    );

    // Execute uploads in parallel with concurrency limit
    let semaphore = Arc::new(Semaphore::new(UPLOAD_CONCURRENCY));
    let storage = Arc::new(BunnyStorage::new(
        project.bunny_storage_zone.clone(),
        project.bunny_storage_password.clone(),
    ));

    let uploads = get_uploads(&deltas);
    let mut upload_tasks = JoinSet::new();

    for delta in uploads {
        let storage = Arc::clone(&storage);
        let path = repo_path.join(&delta.path);
        let remote_path = delta.path.clone();
        let permit = Arc::clone(&semaphore);

        upload_tasks.spawn(async move {
            let _permit = permit.acquire().await;
            upload_with_retry(&storage, &path, &remote_path, MAX_UPLOAD_RETRIES).await
        });
    }

    while let Some(result) = upload_tasks.join_next().await {
        result??;
    }

    // Execute deletions in parallel
    let deletions = get_deletions(&deltas);
    let delete_semaphore = Arc::new(Semaphore::new(DELETE_CONCURRENCY));
    let mut delete_tasks = JoinSet::new();

    for delta in deletions {
        let storage = Arc::clone(&storage);
        let path = delta.path.clone();
        let permit = Arc::clone(&delete_semaphore);

        delete_tasks.spawn(async move {
            let _permit = permit.acquire().await;
            storage.delete_file(&path).await
        });
    }

    while let Some(result) = delete_tasks.join_next().await {
        result??;
    }

    // Delete empty directories after all files are deleted
    // Sort directories by depth (deepest first) to avoid "not empty" errors
    let mut dir_deletions_sorted: Vec<_> = dir_deletions.iter().map(|d| d.path.clone()).collect();
    dir_deletions_sorted.sort_by(|a, b| {
        let a_depth = a.matches('/').count();
        let b_depth = b.matches('/').count();
        b_depth.cmp(&a_depth) // Reverse order: deepest first
    });

    let mut dir_delete_tasks = JoinSet::new();
    for dir_path in dir_deletions_sorted {
        let storage = Arc::clone(&storage);
        dir_delete_tasks.spawn(async move { storage.delete_directory(&dir_path).await });
    }

    let mut successful_dir_deletions = 0;
    while let Some(result) = dir_delete_tasks.join_next().await {
        match result {
            Ok(Ok(())) => successful_dir_deletions += 1,
            Ok(Err(e)) => {
                warn!(event = "dir_delete.failed", error = %e);
            }
            Err(e) => {
                warn!(event = "dir_delete.task_failed", error = %e);
            }
        }
    }

    // Purge CDN cache in parallel
    let purge_urls = get_purge_urls(&deltas, &project.bunny_pull_zone_domain);
    let purged_count = purge_urls.len();

    if !purge_urls.is_empty() {
        let api_key = project
            .bunny_api_key
            .clone()
            .unwrap_or_else(|| api_key.clone());

        let cdn = BunnyCdn::new(api_key);
        let results = cdn
            .purge_urls_parallel(&purge_urls, PURGE_CONCURRENCY)
            .await;

        for (url, result) in results {
            if let Err(e) = result {
                warn!(event = "purge.failed", url = %url, error = %e);
            }
        }
    }

    Ok(DeployStats {
        uploaded: uploaded_count,
        deleted: deleted_count,
        modified: modified_count,
        purged: purged_count,
        skipped: skipped_count,
        dirs_deleted: successful_dir_deletions,
    })
}

async fn upload_with_retry(
    storage: &BunnyStorage,
    path: &std::path::Path,
    remote_path: &str,
    max_retries: u32,
) -> Result<()> {
    let mut last_error = None;

    for attempt in 1..=max_retries {
        match storage.upload_file_from_path(remote_path, path).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(
                    event = "upload.retry",
                    remote_path = %remote_path,
                    attempt = attempt,
                    max_retries = max_retries,
                    error = %e,
                    "upload failed, will retry"
                );
                last_error = Some(e);

                if attempt < max_retries {
                    let delay = RETRY_BASE_DELAY_MS * attempt as u64;
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    }

    Err(last_error
        .map(|e| anyhow::anyhow!("upload failed after {} retries: {}", max_retries, e))
        .unwrap_or_else(|| anyhow::anyhow!("upload failed")))
}

async fn clone_repo(repo_url: &str, dest: &std::path::Path, branch: &str) -> Result<()> {
    tokio::task::spawn_blocking({
        let repo_url = repo_url.to_string();
        let dest = dest.to_path_buf();
        let branch = branch.to_string();
        move || {
            let repo = Repository::init(&dest)?;

            let mut remote = repo.remote("origin", &repo_url)?;

            let mut fetch_opts = FetchOptions::new();
            fetch_opts.depth(1);

            let refspec = format!("refs/heads/{}:refs/remotes/origin/{}", branch, branch);
            remote.fetch(&[&refspec], Some(&mut fetch_opts), None)?;

            let remote_branch_ref = format!("refs/remotes/origin/{}", branch);
            let object = repo.revparse_single(&remote_branch_ref)?;
            let commit = object.peel_to_commit()?;

            repo.checkout_tree(commit.as_object(), None)?;
            repo.set_head_detached(commit.id())?;

            Ok::<_, anyhow::Error>(())
        }
    })
    .await
    .context("clone task panicked")??;

    Ok(())
}

async fn collect_local_files(dir: &std::path::Path) -> Result<LocalFileSet> {
    let mut files = HashMap::new();
    let mut directories = Vec::new();

    // Use blocking walkdir in spawn_blocking for large directories
    let dir_path = dir.to_path_buf();
    let entries: Vec<_> = tokio::task::spawn_blocking(move || {
        WalkDir::new(&dir_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .collect()
    })
    .await
    .context("walkdir task failed")?;

    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(dir)?;
        let key = relative.to_string_lossy().replace('\\', "/");

        // Skip root and .git directory
        if key.is_empty() || key.starts_with(".git/") || key == ".git" {
            continue;
        }

        if entry.file_type().is_dir() {
            directories.push(key);
        } else if entry.file_type().is_file() {
            // Read file async with buffered I/O
            let checksum = compute_file_checksum(path).await?;
            files.insert(key, checksum);
        }
    }

    Ok(LocalFileSet { files, directories })
}

async fn compute_file_checksum(path: &std::path::Path) -> Result<String> {
    let mut file = fs::File::open(path).await.context("failed to open file")?;

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; BUFFER_SIZE];

    loop {
        let n = file
            .read(&mut buffer)
            .await
            .context("failed to read file")?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hex::encode_upper(hasher.finalize()))
}

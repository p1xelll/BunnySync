use crate::bunny::cdn::BunnyCdn;
use crate::bunny::storage::BunnyStorage;
use crate::config::ProjectConfig;
use crate::deploy_queue::DeployQueue;
use crate::diff::{
    compute_delta, count_modified, get_deletions, get_purge_urls, get_skips, get_uploads,
};
use crate::providers::detect_provider;
use crate::signature_cache::SignatureCache;
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use git2::{FetchOptions, Repository};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;

use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

#[derive(Serialize)]
struct DeployResponse {
    status: String,
    uploaded: usize,
    deleted: usize,
    modified: usize,
    purged: usize,
    skipped: usize,
}

impl DeployResponse {
    fn success(
        uploaded: usize,
        deleted: usize,
        modified: usize,
        purged: usize,
        skipped: usize,
    ) -> Self {
        Self {
            status: "deployed".to_string(),
            uploaded,
            deleted,
            modified,
            purged,
            skipped,
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
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    // Check for test mode - only verify signature and return, no deploy
    if params
        .get("test")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false)
    {
        info!(
            event = "webhook.test_mode",
            project_id = %project_id,
            "test mode detected - validating only, no deploy"
        );

        // Still verify the project exists
        if !state.config.projects.contains_key(&project_id) {
            return (StatusCode::NOT_FOUND, "project not found").into_response();
        }

        // Still verify signature
        let provider = match detect_provider(&headers) {
            Some(p) => p,
            None => return (StatusCode::BAD_REQUEST, "unknown provider").into_response(),
        };

        let project = state.config.projects.get(&project_id).unwrap();
        if provider.verify_signature(&body, &headers, &project.webhook_secret).is_err() {
            return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
        }

        return (
            StatusCode::OK,
            "test webhook received - project configured correctly, signature valid",
        )
            .into_response();
    }
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
                "deploy completed successfully"
            );
            let response = DeployResponse::success(
                stats.uploaded,
                stats.deleted,
                stats.modified,
                stats.purged,
                stats.skipped,
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
}

async fn deploy_project(
    project: &ProjectConfig,
    push_event: &crate::providers::PushEvent,
    _api_key: String,
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

    // Debug: Log file counts and sample checksums
    debug!(
        event = "deploy.file_counts",
        local_count = local_files.len(),
        remote_count = remote_files.len(),
        "local vs remote file counts"
    );

    // Log first 5 local files with checksums
    for (i, (path, checksum)) in local_files.iter().take(5).enumerate() {
        debug!(
            event = "deploy.local_file_sample",
            index = i,
            path = %path,
            checksum = %checksum,
            "local file sample"
        );
    }

    // Log first 5 remote files with checksums
    for (i, (path, checksum)) in remote_files.iter().take(5).enumerate() {
        debug!(
            event = "deploy.remote_file_sample",
            index = i,
            path = %path,
            checksum = %checksum,
            "remote file sample"
        );
    }

    let deltas = compute_delta(&local_files, &remote_files);

    // Calculate stats before consuming the vectors
    let uploaded_count = get_uploads(&deltas).len();
    let deleted_count = get_deletions(&deltas).len();
    let modified_count = count_modified(&deltas);
    let skipped_count = get_skips(&deltas).len();

    let uploads = get_uploads(&deltas);
    let deletions = get_deletions(&deltas);

    info!(
        event = "delta.computed",
        local_files = local_files.len(),
        remote_files = remote_files.len(),
        uploads = uploaded_count,
        deletions = deleted_count,
        modified = modified_count,
        skipped = skipped_count,
        "delta computed"
    );

    let semaphore = Arc::new(Semaphore::new(10));
    let storage = BunnyStorage::new(
        project.bunny_storage_zone.clone(),
        project.bunny_storage_password.clone(),
    );

    let mut upload_tasks = JoinSet::new();
    for delta in uploads {
        let storage = storage.clone();
        let path = repo_path.join(&delta.path);
        let remote_path = delta.path.clone();
        let permit = semaphore.clone();

        upload_tasks.spawn(async move {
            let _permit = permit.acquire().await;
            upload_with_retry(&storage, &path, &remote_path, 3).await
        });
    }

    while let Some(result) = upload_tasks.join_next().await {
        result??;
    }

    let mut delete_tasks = JoinSet::new();
    for delta in deletions {
        let storage = storage.clone();
        let path = delta.path.clone();
        delete_tasks.spawn(async move { storage.delete_file(&path).await });
    }

    while let Some(result) = delete_tasks.join_next().await {
        result??;
    }

    let purge_urls = get_purge_urls(&deltas, &project.bunny_pull_zone_domain);

    if !purge_urls.is_empty() {
        let api_key = project
            .bunny_api_key
            .clone()
            .unwrap_or_else(|| std::env::var("BUNNY_API_KEY").unwrap_or_default());

        let cdn = BunnyCdn::new(api_key);

        for url in &purge_urls {
            if let Err(e) = cdn.purge_url(url).await {
                warn!(
                    event = "purge.failed",
                    url = %url,
                    error = %e,
                    "failed to purge URL"
                );
            }
        }
    }

    let purged_count = purge_urls.len();

    Ok(DeployStats {
        uploaded: uploaded_count,
        deleted: deleted_count,
        modified: modified_count,
        purged: purged_count,
        skipped: skipped_count,
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
        let content = fs::read(path).context("failed to read file")?;

        match storage.upload_file(remote_path, &content).await {
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
                    tokio::time::sleep(std::time::Duration::from_millis(100 * attempt as u64))
                        .await;
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

async fn collect_local_files(dir: &std::path::Path) -> Result<HashMap<String, String>> {
    let mut files = HashMap::new();

    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let relative = path.strip_prefix(dir)?;
        let key = relative.to_string_lossy().replace('\\', "/");

        // Skip .git directory
        if key.starts_with(".git/") || key == ".git" {
            continue;
        }

        let content = fs::read(path)?;
        // Use SHA-256 to match Bunny Storage API checksum format
        let checksum = Sha256::digest(&content);
        let checksum = hex::encode_upper(checksum);

        files.insert(key, checksum);
    }

    Ok(files)
}

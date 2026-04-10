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
const BUFFER_SIZE: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Public API surface
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct DeployResponse {
    status: String,
    uploaded: usize,
    deleted: usize,
    modified: usize,
    purged: usize,
    skipped: usize,
    dirs_deleted: usize,
}

impl DeployResponse {
    fn success(stats: &DeployStats) -> Self {
        Self {
            status: "deployed".to_string(),
            uploaded: stats.uploaded,
            deleted: stats.deleted,
            modified: stats.modified,
            purged: stats.purged,
            skipped: stats.skipped,
            dirs_deleted: stats.dirs_deleted,
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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "healthy")
}

async fn handle_webhook(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    info!(event = "webhook.received", project_id = %project_id);

    let project = match state.config.projects.get(&project_id) {
        Some(p) => p,
        None => {
            warn!(event = "webhook.project_not_found", project_id = %project_id);
            return (StatusCode::NOT_FOUND, "project not found").into_response();
        }
    };

    let provider = match detect_provider(&headers) {
        Some(p) => p,
        None => {
            warn!(event = "webhook.unknown_provider");
            return (StatusCode::BAD_REQUEST, "unknown provider").into_response();
        }
    };

    let signature = match provider.verify_signature(&body, &headers, &project.webhook_secret) {
        Ok(sig) => sig,
        Err(e) => {
            warn!(event = "webhook.signature_verification_failed", error = %e);
            return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
        }
    };

    if state.signature_cache.contains(&signature).await {
        warn!(
            event = "webhook.replay_detected",
            project_id = %project_id,
            "replay attack detected — signature already used"
        );
        return (StatusCode::CONFLICT, "duplicate webhook").into_response();
    }

    state.signature_cache.insert(signature).await;

    let push_event = match provider.parse_push_event(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!(event = "webhook.parse_failed", error = %e);
            return (StatusCode::BAD_REQUEST, "invalid payload").into_response();
        }
    };

    // Respond 200 to test webhooks so the provider doesn't flag the endpoint as
    // unhealthy; we just don't act on them.
    if push_event.is_test {
        info!(event = "webhook.test", project_id = %project_id);
        return (
            StatusCode::OK,
            "test webhook received — signature valid, no deploy",
        )
            .into_response();
    }

    // Strip the "refs/heads/" prefix once and reuse everywhere below.
    let webhook_branch = push_event
        .ref_name
        .strip_prefix("refs/heads/")
        .unwrap_or(&push_event.ref_name);

    // Resolve which branch to deploy from:
    // - Use configured DEPLOY_BRANCH if set
    // - Otherwise deploy from the webhook branch (deploy what was pushed)
    let deploy_branch = project.deploy_branch.as_deref().unwrap_or(webhook_branch);

    // Log when we deploy from a different branch than the webhook
    if webhook_branch != deploy_branch {
        info!(
            event = "webhook.branch_override",
            project_id = %project_id,
            webhook_branch = %webhook_branch,
            deploy_branch = %deploy_branch,
        );
    }

    info!(
        event = "deploy.started",
        project_id = %project_id,
        webhook_branch = %webhook_branch,
        deploy_branch = %deploy_branch,
        commit = %push_event.commit,
    );

    let _deploy_permit: OwnedSemaphorePermit = match state.deploy_queue.acquire(&project_id).await {
        Some(permit) => permit,
        None => {
            warn!(event = "deploy.already_in_progress", project_id = %project_id);
            return (StatusCode::CONFLICT, "deploy already in progress").into_response();
        }
    };

    let api_key = project
        .bunny_api_key
        .clone()
        .unwrap_or_else(|| state.config.bunny_api_key.clone());

    match deploy_project(project, deploy_branch, api_key).await {
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
            );
            (StatusCode::OK, Json(DeployResponse::success(&stats))).into_response()
        }
        Err(e) => {
            error!(event = "deploy.failed", project_id = %project_id, error = %e);
            (StatusCode::INTERNAL_SERVER_ERROR, "deploy failed").into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Deploy pipeline
// ---------------------------------------------------------------------------

async fn deploy_project(
    project: &ProjectConfig,
    branch: &str,
    api_key: String,
) -> Result<DeployStats> {
    let temp_dir = TempDir::new().context("failed to create temp directory")?;
    let repo_path = temp_dir.path().join("repo");

    clone_repo(&project.repo_url, &repo_path, branch).await?;

    let local_files = collect_local_files(&repo_path).await?;

    let storage = Arc::new(BunnyStorage::new(
        project.bunny_storage_zone.clone(),
        project.bunny_storage_password.clone(),
    ));

    let remote_files = storage.list_files("").await?;

    debug!(
        event = "deploy.file_counts",
        local_files = local_files.files.len(),
        remote_files = remote_files.files.len(),
        local_dirs = local_files.directories.len(),
        remote_dirs = remote_files.directories.len(),
    );

    let deltas = compute_delta(
        &local_files.files,
        &remote_files.files,
        &local_files.directories,
        &remote_files.directories,
    );

    let uploaded_count = get_uploads(&deltas).len();
    let deleted_count = get_deletions(&deltas).len();
    let modified_count = count_modified(&deltas);
    let skipped_count = get_skips(&deltas).len();
    let dir_deletions = get_dir_deletions(&deltas);
    let dir_deletion_count = dir_deletions.len();

    info!(
        event = "delta.computed",
        uploads = uploaded_count,
        deletions = deleted_count,
        dir_deletions = dir_deletion_count,
        modified = modified_count,
        skipped = skipped_count,
    );

    // --- uploads -----------------------------------------------------------

    let upload_semaphore = Arc::new(Semaphore::new(UPLOAD_CONCURRENCY));
    let mut upload_tasks: JoinSet<Result<()>> = JoinSet::new();

    for delta in get_uploads(&deltas) {
        let storage = Arc::clone(&storage);
        let path = repo_path.join(&delta.path);
        let remote_path = delta.path.clone();
        let sem = Arc::clone(&upload_semaphore);

        upload_tasks.spawn(async move {
            let _permit = sem.acquire().await;
            upload_with_retry(&storage, &path, &remote_path, MAX_UPLOAD_RETRIES).await
        });
    }

    while let Some(result) = upload_tasks.join_next().await {
        result??;
    }

    // --- file deletions ----------------------------------------------------

    let delete_semaphore = Arc::new(Semaphore::new(DELETE_CONCURRENCY));
    let mut delete_tasks: JoinSet<Result<()>> = JoinSet::new();

    for delta in get_deletions(&deltas) {
        let storage = Arc::clone(&storage);
        let path = delta.path.clone();
        let sem = Arc::clone(&delete_semaphore);

        delete_tasks.spawn(async move {
            let _permit = sem.acquire().await;
            storage.delete_file(&path).await
        });
    }

    while let Some(result) = delete_tasks.join_next().await {
        result??;
    }

    // --- directory deletions -----------------------------------------------
    // Process deepest paths first so we never hit "directory not empty".

    let mut dir_paths: Vec<String> = dir_deletions.iter().map(|d| d.path.clone()).collect();
    dir_paths.sort_unstable_by_key(|p| std::cmp::Reverse(p.matches('/').count()));

    let mut dir_delete_tasks: JoinSet<Result<()>> = JoinSet::new();
    for dir_path in dir_paths {
        let storage = Arc::clone(&storage);
        dir_delete_tasks.spawn(async move { storage.delete_directory(&dir_path).await });
    }

    let mut successful_dir_deletions: usize = 0;
    while let Some(result) = dir_delete_tasks.join_next().await {
        match result {
            Ok(Ok(())) => successful_dir_deletions += 1,
            Ok(Err(e)) => warn!(event = "dir_delete.failed", error = %e),
            Err(e) => warn!(event = "dir_delete.task_panicked", error = %e),
        }
    }

    // --- CDN purge ---------------------------------------------------------

    let purge_urls = get_purge_urls(&deltas, &project.bunny_pull_zone_domain);
    let purged_count = purge_urls.len();

    if !purge_urls.is_empty() {
        let cdn_api_key = project
            .bunny_api_key
            .clone()
            .unwrap_or_else(|| api_key.clone());

        let cdn = BunnyCdn::new(cdn_api_key);
        for (url, result) in cdn
            .purge_urls_parallel(&purge_urls, PURGE_CONCURRENCY)
            .await
        {
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

// ---------------------------------------------------------------------------
// Git
// ---------------------------------------------------------------------------

async fn clone_repo(repo_url: &str, dest: &std::path::Path, branch: &str) -> Result<()> {
    tokio::task::spawn_blocking({
        let repo_url = repo_url.to_string();
        let dest = dest.to_path_buf();
        let branch = branch.to_string();

        move || {
            std::fs::create_dir_all(&dest)?;

            let url = gix::url::parse(repo_url.as_str().into())
                .context("failed to parse repository URL")?;

            // Fetch only the single branch we care about — no reason to pull
            // the entire remote ref namespace for a deploy pipeline.
            let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");

            let prepare_clone = gix::prepare_clone(url, &dest)
                .context("failed to prepare clone")?
                // with_ref_name() tells gix which ref to check out via
                // main_worktree(), available since gix 0.64.
                .with_ref_name(Some(branch.as_str()))
                .with_context(|| format!("invalid ref name '{branch}'"))?;

            // Replace the default wildcard refspec with our single-branch one.
            let mut prepare_clone = prepare_clone.configure_remote(move |mut remote| {
                remote = remote
                    .with_refspecs(Some(refspec.as_str()), gix::remote::Direction::Fetch)
                    .context("failed to configure single-branch refspec")?;
                Ok(remote)
            });

            let (mut prepare_checkout, _outcome) = prepare_clone
                .fetch_then_checkout(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
                .with_context(|| format!("failed to fetch branch '{branch}'"))?;

            // main_worktree() honours the ref set above and performs the
            // checkout — no manual tree walk required.
            let (_repo, _outcome) = prepare_checkout
                .main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
                .context("failed to checkout worktree")?;

            Ok::<_, anyhow::Error>(())
        }
    })
    .await
    .context("clone task panicked")??;

    Ok(())
}

// ---------------------------------------------------------------------------
// Storage helpers
// ---------------------------------------------------------------------------

async fn upload_with_retry(
    storage: &BunnyStorage,
    path: &std::path::Path,
    remote_path: &str,
    max_retries: u32,
) -> Result<()> {
    let mut last_err = None;

    for attempt in 1..=max_retries {
        match storage.upload_file_from_path(remote_path, path).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(
                    event = "upload.retry",
                    remote_path = %remote_path,
                    attempt,
                    max_retries,
                    error = %e,
                );
                last_err = Some(e);

                if attempt < max_retries {
                    tokio::time::sleep(Duration::from_millis(
                        RETRY_BASE_DELAY_MS * u64::from(attempt),
                    ))
                    .await;
                }
            }
        }
    }

    Err(last_err
        .map(|e| anyhow::anyhow!("upload failed after {max_retries} retries: {e}"))
        .unwrap_or_else(|| anyhow::anyhow!("upload failed")))
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

async fn collect_local_files(dir: &std::path::Path) -> Result<LocalFileSet> {
    // Collect directory entries on a blocking thread — WalkDir does synchronous
    // syscalls and we don't want to hold the async executor.
    let dir_path = dir.to_path_buf();
    let entries: Vec<_> = tokio::task::spawn_blocking(move || {
        WalkDir::new(&dir_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .collect()
    })
    .await
    .context("walkdir task panicked")?;

    // Compute checksums in parallel, bounded by the same concurrency limit we
    // use for uploads — both are I/O bound against local disk.
    let semaphore = Arc::new(Semaphore::new(UPLOAD_CONCURRENCY));
    let mut checksum_tasks: JoinSet<Result<Option<(String, String)>>> = JoinSet::new();
    let mut directories: Vec<String> = Vec::new();

    for entry in &entries {
        let relative = entry.path().strip_prefix(dir)?;
        let key = relative.to_string_lossy().replace('\\', "/");

        if key.is_empty() || key == ".git" || key.starts_with(".git/") {
            continue;
        }

        if entry.file_type().is_dir() {
            directories.push(key);
        } else if entry.file_type().is_file() {
            let path = entry.path().to_path_buf();
            let sem = Arc::clone(&semaphore);

            checksum_tasks.spawn(async move {
                let _permit = sem.acquire().await;
                let checksum = compute_file_checksum(&path).await?;
                Ok(Some((key, checksum)))
            });
        }
    }

    let mut files = HashMap::with_capacity(checksum_tasks.len());
    while let Some(result) = checksum_tasks.join_next().await {
        if let Some((key, checksum)) = result?? {
            files.insert(key, checksum);
        }
    }

    Ok(LocalFileSet { files, directories })
}

async fn compute_file_checksum(path: &std::path::Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .await
        .with_context(|| format!("failed to open '{}'", path.display()))?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; BUFFER_SIZE];

    loop {
        let n = file.read(&mut buf).await.context("failed to read file")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hex::encode_upper(hasher.finalize()))
}

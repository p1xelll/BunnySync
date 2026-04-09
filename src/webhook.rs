use crate::bunny::cdn::BunnyCdn;
use crate::bunny::storage::BunnyStorage;
use crate::config::ProjectConfig;
use crate::deploy_queue::DeployQueue;
use crate::diff::{compute_delta, get_deletions, get_purge_urls, get_uploads};
use crate::providers::detect_provider;
use crate::signature_cache::SignatureCache;
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use git2::{FetchOptions, Repository};
use std::collections::HashMap;
use std::fs;

use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tracing::{error, info, warn};

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
) -> impl IntoResponse {
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
            return (StatusCode::NOT_FOUND, "project not found");
        }
    };

    let provider = match detect_provider(&headers) {
        Some(p) => p,
        None => {
            warn!(event = "webhook.unknown_provider", "unknown provider");
            return (StatusCode::BAD_REQUEST, "unknown provider");
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
            return (StatusCode::UNAUTHORIZED, "invalid signature");
        }
    };

    if state.signature_cache.contains(&signature).await {
        warn!(
            event = "webhook.replay_detected",
            project_id = %project_id,
            "replay attack detected - signature already used"
        );
        return (StatusCode::CONFLICT, "duplicate webhook");
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
            return (StatusCode::BAD_REQUEST, "invalid payload");
        }
    };

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
            return (StatusCode::CONFLICT, "deploy already in progress");
        }
    };

    if let Err(e) = deploy_project(project, &push_event).await {
        error!(
            event = "deploy.failed",
            project_id = %project_id,
            error = %e,
            "deploy failed"
        );
        return (StatusCode::INTERNAL_SERVER_ERROR, "deploy failed");
    }

    info!(
        event = "deploy.completed",
        project_id = %project_id,
        "deploy completed successfully"
    );
    (StatusCode::OK, "deployed")
}

async fn deploy_project(
    project: &ProjectConfig,
    push_event: &crate::providers::PushEvent,
) -> Result<()> {
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

    let deltas = compute_delta(&local_files, &remote_files);

    let uploads = get_uploads(&deltas);
    let deletions = get_deletions(&deltas);

    info!(
        event = "delta.computed",
        uploads = uploads.len(),
        deletions = deletions.len(),
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

    Ok(())
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

        let content = fs::read(path)?;
        let checksum = format!("{:x}", md5::compute(&content));

        files.insert(key, checksum);
    }

    Ok(files)
}

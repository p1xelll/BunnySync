# ![BunnySync Logo](docs/logo.svg) BunnySync

A webhook receiver that automatically deploys Git repositories to BunnyCDN Storage zones. Supports Forgejo (Codeberg), Tangled, GitHub, and GitLab webhooks with automatic CDN cache purging.

[![Docker Pulls](https://img.shields.io/docker/pulls/p1xelll/bunnysync?style=flat-square&logo=docker)](https://hub.docker.com/r/p1xelll/bunnysync)

## Features

- **Webhook Support**: Receives push events from Forgejo (Codeberg), Tangled, GitHub, and GitLab
- **Automatic Deployments**: Clones repo, computes delta, uploads changed files
- **CDN Integration**: Automatically purges BunnyCDN cache for modified files
- **Multi-Architecture**: Docker images available for AMD64 and ARM64
- **Concurrent Operations**: Parallel file uploads/deletions with configurable limits
- **Signature Verification**: HMAC-SHA256 webhook signature validation
- **Replay Protection**: Signature deduplication prevents replay attacks
- **Queue Management**: Per-project deployment queue prevents concurrent deploys

## Quick Start

### Docker Hub

Pull the latest image:

```bash
docker pull p1xelll/bunnysync:latest
```

Run with environment variables:

```bash
docker run -d \
  -p 3000:3000 \
  -e BIND_ADDR=0.0.0.0:3000 \
  -e BUNNY_API_KEY=your-bunny-api-key \
  -e PROJECT_MYAPP_REPO_URL=https://git.example.com/user/myapp.git \
  -e PROJECT_MYAPP_WEBHOOK_SECRET=your-webhook-secret-min-32-chars \
  -e PROJECT_MYAPP_BUNNY_STORAGE_ZONE=myapp-zone \
  -e PROJECT_MYAPP_BUNNY_STORAGE_PASSWORD=zone-password \
  -e PROJECT_MYAPP_BUNNY_PULL_ZONE_ID=123456 \
  -e PROJECT_MYAPP_BUNNY_PULL_ZONE_DOMAIN=cdn.example.com \
  p1xelll/bunnysync:latest
```

## Docker Compose

The easiest way to run BunnySync is with Docker Compose. See the included [`docker-compose.yml`](docker-compose.yml) file for a complete example.

### .env.example

```bash
# Copy to .env and fill in your values

# Global BunnyCDN API Key (for cache purging)
BUNNY_API_KEY=your-global-bunny-api-key

# Project: MyApp
MYAPP_WEBHOOK_SECRET=generate-a-random-secret-min-32-characters
MYAPP_STORAGE_PASSWORD=your-storage-zone-password
MYAPP_BUNNY_API_KEY=optional-project-specific-api-key

# Project: Website (optional)
WEBSITE_WEBHOOK_SECRET=another-random-secret-min-32-characters
WEBSITE_STORAGE_PASSWORD=your-storage-zone-password
```

### Start the service

```bash
# Download compose file
wget https://codeberg.org/p1xel/BunnySync/raw/branch/main/docker-compose.yml

# Create environment file
cp .env.example .env
# Edit .env with your values

# Start
docker-compose up -d

# View logs
docker-compose logs -f bunnysync
```

## Configuration

### Environment Variables

#### Global Settings

| Variable | Required | Description |
|----------|----------|-------------|
| `BIND_ADDR` | Yes | Server bind address (e.g., `0.0.0.0:3000`) |
| `BUNNY_API_KEY` | Yes | Global BunnyCDN API key for cache purging |

#### Project Settings (per project)

Replace `{PROJECT_ID}` with your project identifier (uppercase, alphanumeric + underscore):

| Variable | Required | Description |
|----------|----------|-------------|
| `PROJECT_{PROJECT_ID}_REPO_URL` | Yes | Git repository URL (HTTPS) |
| `PROJECT_{PROJECT_ID}_WEBHOOK_SECRET` | Yes | Webhook secret (min 32 chars) |
| `PROJECT_{PROJECT_ID}_BUNNY_STORAGE_ZONE` | Yes | Bunny Storage zone name |
| `PROJECT_{PROJECT_ID}_BUNNY_STORAGE_PASSWORD` | Yes | Storage zone password |
| `PROJECT_{PROJECT_ID}_BUNNY_PULL_ZONE_ID` | Yes | Pull zone ID (number) |
| `PROJECT_{PROJECT_ID}_BUNNY_PULL_ZONE_DOMAIN` | Yes | Pull zone domain (e.g., `cdn.example.com`) |
| `PROJECT_{PROJECT_ID}_BUNNY_API_KEY` | No | Project-specific API key (overrides global) |
| `PROJECT_{PROJECT_ID}_DEPLOY_BRANCH` | No | Branch to deploy from (e.g., `main`). If set, always deploys this branch regardless of which branch triggered the webhook. If not set, deploys from the webhook branch |

#### DEPLOY_BRANCH Behavior

The `DEPLOY_BRANCH` variable controls which branch is deployed when a webhook is received:

| Webhook Branch | DEPLOY_BRANCH | Deploys From | Description |
|----------------|---------------|--------------|-------------|
| `main` | `pages` | `pages` | Always deploys the configured branch |
| `pages` | - | `pages` | Deploys from webhook branch (no override) |
| `docs` | `main` | `main` | Configured branch takes precedence |

**Key points:**
- If `DEPLOY_BRANCH` is set, **always** deploy that branch (ignores webhook branch)
- If `DEPLOY_BRANCH` is not set, deploy from whichever branch triggered the webhook
- Useful for platforms like Tangled that don't support branch filtering in webhook settings

### Example: Multiple Projects

```bash
# Project: blog
PROJECT_BLOG_REPO_URL=https://git.example.com/user/blog.git
PROJECT_BLOG_WEBHOOK_SECRET=blog-secret-min-32-characters-long
PROJECT_BLOG_BUNNY_STORAGE_ZONE=blog-storage
PROJECT_BLOG_BUNNY_STORAGE_PASSWORD=zone-password-here
PROJECT_BLOG_BUNNY_PULL_ZONE_ID=111111
PROJECT_BLOG_BUNNY_PULL_ZONE_DOMAIN=blog.example.com

# Project: shop
PROJECT_SHOP_REPO_URL=https://git.example.com/user/shop.git
PROJECT_SHOP_WEBHOOK_SECRET=shop-secret-min-32-characters-long
PROJECT_SHOP_BUNNY_STORAGE_ZONE=shop-storage
PROJECT_SHOP_BUNNY_STORAGE_PASSWORD=another-zone-password
PROJECT_SHOP_BUNNY_PULL_ZONE_ID=222222
PROJECT_SHOP_BUNNY_PULL_ZONE_DOMAIN=shop.example.com
```

## Webhook Setup

### Forgejo (Codeberg)

1. Go to your repository → **Settings → Webhooks**
2. Add a new webhook and select **Forgejo** type
3. Set **Target URL**: `http://your-server:3000/hook/{PROJECT_ID}`
4. Set **HTTP Method** to **POST**
5. Set **Content Type** to `application/json`
6. Set **Secret** to match `PROJECT_{PROJECT_ID}_WEBHOOK_SECRET`
7. Optionally set **Branch filter** to limit which branches trigger the webhook (e.g., `main` or `docs`)
8. Trigger on **Push events** and save

### Tangled

1. Go to your repository → **Settings → Hooks**
2. Click **new webhook**
3. Set **Payload URL**: `http://your-server:3000/hook/{PROJECT_ID}`
4. Set **Secret** to match `PROJECT_{PROJECT_ID}_WEBHOOK_SECRET`
5. Select **Push events** and save

Note: Tangled automatically sends `application/json` content type and does not require manual configuration.

### GitHub

1. Go to your repository → **Settings → Webhooks**
2. Click **Add webhook**
3. Set **Payload URL**: `http://your-server:3000/hook/{PROJECT_ID}`
4. Set **Content type** to `application/json`
5. Set **Secret** to match `PROJECT_{PROJECT_ID}_WEBHOOK_SECRET`
6. Choose **Just the push event**
7. Click **Add webhook**

Note: GitHub automatically includes the signature in the `X-Hub-Signature-256` header using HMAC-SHA256.

### GitLab

1. Go to your repository → **Settings → Webhooks**
2. Click **Add new webhook**
3. Set **URL**: `http://your-server:3000/hook/{PROJECT_ID}`
4. Set **Secret token** to match `PROJECT_{PROJECT_ID}_WEBHOOK_SECRET`
5. Select **Push events** trigger
6. Optionally configure **Wildcard pattern** to limit which branches trigger the webhook (e.g., `main` or `release/*`)
7. Click **Add webhook**

Note: GitLab sends the secret token in the `X-Gitlab-Token` header. Webhook deduplication uses GitLab's `Idempotency-Key` header to prevent replay attacks while allowing legitimate retries.


## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check endpoint |
| `/hook/{project_id}` | POST | Webhook receiver |

### Webhook Response

**Success (200 OK):**
```json
{
  "status": "deployed",
  "uploaded": 5,
  "deleted": 2,
  "modified": 3,
  "purged": 3,
  "skipped": 42,
  "dirs_deleted": 1
}
```

**Errors:**
- `400` - Unknown provider or invalid payload
- `401` - Invalid signature
- `404` - Project not found
- `409` - Deploy already in progress or duplicate webhook
- `500` - Internal server error

## Architecture

The application is built with:
- **Rust** with Tokio async runtime
- **Axum** web framework
- **gix** (pure Rust Git implementation) for Git operations
- **reqwest** for HTTP requests

### Performance Features

- Connection pooling for HTTP clients
- Parallel file uploads (default: 10 concurrent)
- Parallel file deletions (default: 10 concurrent)
- Parallel CDN purging (default: 5 concurrent)
- Streaming file reads with 64KB buffers
- Efficient delta computation using SHA-256 checksums

## Adding a New Provider

BunnySync uses a provider system to support different Git hosting platforms. Currently supported:
- **GitHub** (world's largest Git hosting platform)
- **GitLab** (popular open-source DevOps platform, gitlab.com and self-hosted)
- **Forgejo** (used by Codeberg)
- **Tangled** (decentralized Git hosting on AT Protocol)

To add a new provider:

### 1. Fork and clone

```bash
# Fork the repository on Codeberg first, then clone your fork
git clone https://codeberg.org/p1xel/BunnySync.git
cd bunnysync
```

### 2. Create the provider file

Create a new file in `src/providers/{provider_name}.rs` implementing the `GitProvider` trait:

```rust
use super::{GitProvider, PushEvent};
use anyhow::{Context, Result, anyhow};
use axum::http::HeaderMap;

pub struct MyProvider;

impl GitProvider for MyProvider {
    fn verify_signature(&self, payload: &[u8], headers: &HeaderMap, secret: &str) -> Result<String> {
        // Extract and verify webhook signature
        // Return signature string for deduplication cache
        todo!()
    }

    fn parse_push_event(&self, payload: &[u8]) -> Result<PushEvent> {
        // Parse JSON payload and extract:
        // - ref_name: Git reference (e.g., "refs/heads/main")
        // - commit: The new commit SHA
        // - is_test: true if before == after (test webhook)
        todo!()
    }
}
```

See existing providers (`src/providers/forgejo.rs`, `src/providers/tangled.rs`) for full implementation examples.

### 3. Register the provider

Update `src/providers/mod.rs` to add your provider:

```rust
pub mod forgejo;
pub mod github;
pub mod tangled;
pub mod myprovider;  // Add this line

pub fn detect_provider(headers: &HeaderMap) -> Option<Box<dyn GitProvider>> {
    // Check for Forgejo first (Codeberg uses Forgejo - priority platform)
    if headers.contains_key("X-Forgejo-Event") {
        Some(Box::new(forgejo::ForgejoProvider))
    }
    // Check for Tangled
    else if headers.contains_key("X-Tangled-Event") {
        Some(Box::new(tangled::TangledProvider))
    }
    // Check for GitHub
    else if headers.contains_key("X-GitHub-Event") {
        Some(Box::new(github::GithubProvider))
    }
    // Add your provider here
    else if headers.contains_key("X-MyProvider-Event") {
        Some(Box::new(myprovider::MyProvider))
    }
    // No matching provider found
    else {
        None
    }
}
```

### 4. Test your provider

1. Build and run locally: `cargo run --release`
2. Add a webhook in your Git hosting platform
3. Point it to `http://localhost:3000/hook/{PROJECT_ID}`
4. Trigger a push event

### 5. Submit your changes

Once your provider is working:

1. **Run tests and linting**:
   ```bash
   cargo test
   cargo clippy -- -D warnings
   cargo fmt
   ```
2. **Push to your fork**:
   ```bash
   git add .
   git commit -m "Add MyProvider support"
   git push origin main
   ```
3. **Create a pull request** with:
   - Description of the Git platform supported
   - Link to webhook documentation
   - Any special configuration notes

## Building from Source

> **Note:** These requirements are only needed when building from source. Docker users don't need to install anything.

### Requirements

- **Rust** 1.85+

No other dependencies required - BunnySync is written in pure Rust with no C library dependencies.

On Debian/Ubuntu:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

On macOS:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Build

```bash
# Clone repository
git clone https://codeberg.org/p1xel/BunnySync.git
cd bunnysync

# Build release binary
cargo build --release

# Binary will be at:
# target/release/bunnysync
```

### Run locally

```bash
export BIND_ADDR=0.0.0.0:3000
export BUNNY_API_KEY=your-api-key
export PROJECT_MYAPP_REPO_URL=https://git.example.com/user/myapp.git
# ... other env vars

cargo run --release
```

## Docker Build

### Local build

```bash
docker build -t bunnysync:local .
```

## License

MIT License - see LICENSE file for details

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Check compilation: `cargo check`
5. Format code: `cargo fmt`
6. Run linting: `cargo clippy -- -D warnings`
7. Run tests: `cargo test`
8. Build release: `cargo build --release`
9. Submit a pull request

## Support

- Issues: https://codeberg.org/p1xel/BunnySync/issues

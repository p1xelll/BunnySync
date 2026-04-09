# BunnySync

A webhook receiver that automatically deploys Git repositories to BunnyCDN Storage zones. Supports Forgejo webhooks with automatic CDN cache purging.

**Docker Hub:** https://hub.docker.com/r/p1xelll/bunnysync

## Features

- **Webhook Support**: Receives push events from Forgejo
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

### docker-compose.yml

```yaml
version: '3.8'

services:
  bunnysync:
    image: p1xelll/bunnysync:latest
    container_name: bunnysync
    restart: unless-stopped
    ports:
      - "3000:3000"
    environment:
      # Server configuration
      - BIND_ADDR=0.0.0.0:3000
      - BUNNY_API_KEY=${BUNNY_API_KEY}
      
      # Project 1: MyApp
      - PROJECT_MYAPP_REPO_URL=https://git.example.com/user/myapp.git
      - PROJECT_MYAPP_WEBHOOK_SECRET=${MYAPP_WEBHOOK_SECRET}
      - PROJECT_MYAPP_BUNNY_STORAGE_ZONE=myapp-storage
      - PROJECT_MYAPP_BUNNY_STORAGE_PASSWORD=${MYAPP_STORAGE_PASSWORD}
      - PROJECT_MYAPP_BUNNY_PULL_ZONE_ID=123456
      - PROJECT_MYAPP_BUNNY_PULL_ZONE_DOMAIN=cdn.example.com
      - PROJECT_MYAPP_BUNNY_API_KEY=${MYAPP_BUNNY_API_KEY}
      
      # Project 2: Website (optional)
      - PROJECT_WEBSITE_REPO_URL=https://git.example.com/user/website.git
      - PROJECT_WEBSITE_WEBHOOK_SECRET=${WEBSITE_WEBHOOK_SECRET}
      - PROJECT_WEBSITE_BUNNY_STORAGE_ZONE=website-storage
      - PROJECT_WEBSITE_BUNNY_STORAGE_PASSWORD=${WEBSITE_STORAGE_PASSWORD}
      - PROJECT_WEBSITE_BUNNY_PULL_ZONE_ID=789012
      - PROJECT_WEBSITE_BUNNY_PULL_ZONE_DOMAIN=www.example.com
    healthcheck:
      test: ["CMD", "wget", "--quiet", "--tries=1", "--spider", "http://localhost:3000/health"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 5s
```

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

## Webhook Setup (Forgejo)

1. Go to your repository → Settings → Webhooks
2. Add a new Forgejo webhook
3. Target URL: `http://your-server:3000/hook/{PROJECT_ID}`
   - Example: `http://bunnysync.example.com:3000/hook/MYAPP`
4. HTTP Method: `POST`
5. Secret: Use the same value as `PROJECT_{PROJECT_ID}_WEBHOOK_SECRET`
6. Trigger on: **Push events**
7. Save

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
- **libgit2** for Git operations
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
- **Forgejo**

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
    fn verify_signature(
        &self,
        payload: &[u8],
        headers: &HeaderMap,
        secret: &str,
    ) -> Result<String> {
        // Extract signature from headers
        let signature = headers
            .get("X-MyProvider-Signature")
            .ok_or_else(|| anyhow!("missing signature header"))?
            .to_str()
            .context("invalid signature header")?;

        // Verify HMAC-SHA256 signature
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| anyhow!("invalid secret: {}", e))?;
        mac.update(payload);

        let expected = hex::decode(signature).context("invalid signature hex")?;

        mac.verify_slice(&expected)
            .map_err(|_| anyhow!("signature verification failed"))?;

        Ok(signature.to_string())
    }

    fn parse_push_event(&self, payload: &[u8]) -> Result<PushEvent> {
        let json: serde_json::Value = serde_json::from_slice(payload)
            .context("invalid JSON")?;

        let ref_name = json
            .get("ref")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing ref"))?
            .to_string();

        let before = json.get("before").and_then(|v| v.as_str()).unwrap_or("");

        let after = json
            .get("after")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Detect test webhook: before and after are the same
        let is_test = before == after && !before.is_empty();

        Ok(PushEvent {
            ref_name,
            commit: after.to_string(),
            is_test,
        })
    }
}
```

### 3. Register the provider

Update `src/providers/mod.rs` to add your provider:

```rust
pub mod forgejo;
pub mod myprovider;  // Add this line

pub fn detect_provider(headers: &HeaderMap) -> Option<Box<dyn GitProvider>> {
    if headers.contains_key("X-Forgejo-Event") || headers.contains_key("X-Gitea-Event") {
        Some(Box::new(forgejo::ForgejoProvider))
    } else if headers.contains_key("X-MyProvider-Event") {  // Add this
        Some(Box::new(myprovider::MyProvider))
    } else {
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

- **Rust** 1.85+ (for the application)
- **CMake** (required by the `git2` crate to compile libgit2)
- **OpenSSL** development libraries (`libssl-dev` on Debian/Ubuntu)
- **pkg-config** (to locate system libraries)
- **Git** (for repository operations)

On Debian/Ubuntu:
```bash
sudo apt-get update && sudo apt-get install -y cmake libssl-dev pkg-config git
```

On macOS:
```bash
brew install cmake openssl pkg-config git
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

### Multi-arch build with buildx

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -t p1xelll/bunnysync:latest \
  --push .
```

## License

MIT License - see LICENSE file for details

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run tests: `cargo test`
5. Run linting: `cargo clippy -- -D warnings`
6. Format code: `cargo fmt`
7. Submit a pull request

## Support

- Issues: https://codeberg.org/p1xel/BunnySync/issues

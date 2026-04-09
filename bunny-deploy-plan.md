# bunny-deploy

Self-hosted webhook server that syncs static sites to Bunny Storage on push. Written in Rust, deployable anywhere Docker runs. Fully stateless — no database, no registration. Supports multiple projects per instance.

---

## What it does

1. Receives a webhook push event from Forgejo (or GitHub, GitLab, Gitea)
2. Extracts `project_id` from the URL path `/hook/:project_id`
3. Looks up project config from in-memory map — returns `404` if project not found
4. Verifies the HMAC signature using the project's secret
5. Shallow-clones the project's repo into a temp directory
6. Fetches the remote file listing from Bunny Storage with checksums
7. Computes a delta — upload new/changed, delete removed, skip unchanged
8. Uploads the delta in parallel
9. Purges only the changed URLs via Bunny CDN API (no full-zone purge)
10. Cleans up temp directory

---

## Deploy flow

```
POST /hook/blog
       │
       │  project_id = "blog"
       │  X-Forgejo-Signature: sha256={hmac}
       ▼
┌──────────────────────────────────────────┐
│               bunny-deploy                │
│                                           │
│  1. extract project_id from URL           │
│  2. lookup in-memory HashMap              │
│     → 404 if project not found            │
│  3. verify HMAC (constant-time)           │
│     → 401 if invalid                      │
│  4. resolve BUNNY_API_KEY                 │
│     (per-project or fallback to global)   │
│  5. git clone REPO_URL                    │
│  6. diff vs Bunny Storage                 │
│  7. upload delta           ───────────────┼──► Bunny Storage
│  8. delete stale           ───────────────┼──► Bunny Storage
│  9. purge changed URLs     ───────────────┼──► Bunny CDN API
│  10. cleanup temp dir                     │
└──────────────────────────────────────────┘
```

---

## Diff and purge logic

The server compares local files (from git clone) against remote files (from Bunny Storage listing) using MD5 checksums.

```
Local (git clone):           Bunny Storage:
──────────────────           ──────────────
index.html   MD5=aaa         index.html   MD5=aaa
about.html   MD5=bbb         about.html   MD5=xxx  ← changed
contact.html MD5=ccc         old-page.html MD5=zzz ← missing locally
                             ← new file, not on Bunny yet
```

Each file falls into one of four categories:

| Action | Condition | Upload | Delete | Purge cache |
|---|---|---|---|---|
| `SKIP` | MD5 identical | ✗ | ✗ | ✗ |
| `UPLOAD (changed)` | MD5 different | ✓ | ✗ | ✓ CDN has stale version |
| `UPLOAD (new)` | exists locally, not on Bunny | ✓ | ✗ | ✗ CDN has never seen it |
| `DELETE` | exists on Bunny, not locally | ✗ | ✓ | ✓ CDN still has it cached |

New files do not need a cache purge — the CDN has never cached them, there is nothing to invalidate.

---

## Startup — config loading and validation

At startup, before the HTTP server starts, the server:

1. Reads all env vars once via `std::env::vars()`
2. Finds all vars starting with `PROJECT_` and builds a `HashMap<String, ProjectConfig>`
3. Validates the entire config — panics with a clear error if anything is missing
4. Passes the config into Axum `State` — every request reads from this in-memory map

Env vars are **never read at request time**. `std::env::var()` is not called inside any request handler — only the pre-built `HashMap` from `State` is accessed.

```rust
// config.rs
pub struct ProjectConfig {
    pub repo_url: String,
    pub webhook_secret: String,       // min 32 chars
    pub bunny_storage_zone: String,
    pub bunny_storage_password: String,
    pub bunny_pull_zone_id: String,
    pub bunny_api_key: Option<String>, // None = use global fallback
}

pub struct Config {
    pub bind_addr: String,
    pub bunny_api_key: String,         // global fallback
    pub projects: HashMap<String, ProjectConfig>,
}

// main.rs
let config = Config::from_env();  // reads + validates once
config.validate();                 // panics if invalid
let app = Router::new()
    .with_state(Arc::new(config)); // passed into Axum State
```

### Startup output

Success:
```
[startup] validating config...
[startup] found projects: FILMLOG, BLOG
[startup] FILMLOG: ok
[startup] BLOG: ok
[startup] global BUNNY_API_KEY: ok
[startup] config valid — starting server on 0.0.0.0:3000
```

Failure:
```
[startup] validating config...
[startup] found projects: FILMLOG, BLOG
[startup] FILMLOG: ok
[startup] BLOG: missing PROJECT_BLOG_BUNNY_PULL_ZONE_ID

FATAL: invalid configuration — fix the above errors and restart
```

### What is validated at startup

- `BIND_ADDR` is set and is a valid address
- `BUNNY_API_KEY` is set (global)
- At least one project is configured
- For each discovered project:
  - `PROJECT_{ID}_REPO_URL` is set and is a valid URL
  - `PROJECT_{ID}_WEBHOOK_SECRET` is set and is at least 32 characters
  - `PROJECT_{ID}_BUNNY_STORAGE_ZONE` is set
  - `PROJECT_{ID}_BUNNY_STORAGE_PASSWORD` is set
  - `PROJECT_{ID}_BUNNY_PULL_ZONE_ID` is set and is a valid number

---

## Security model

### Webhook secret length

`WEBHOOK_SECRET` must be at least 32 characters (256 bits of entropy when random). This is the industry standard used by GitHub, Stripe, and others for HMAC-SHA256. A shorter secret would be faster to brute-force.

The server enforces this at startup — it will not start with a shorter secret.

### Per-project HMAC secrets

Each project has its own `WEBHOOK_SECRET`. If one secret leaks, other projects are not affected.

`project_id` is not a secret — it is only a selector to look up the correct config from the in-memory map. Real authentication happens in the next step when HMAC is verified.

```
POST /hook/blog
  → project_id = "blog"
  → lookup in HashMap → ProjectConfig { webhook_secret: "abc123..." }
  → verify HMAC with "abc123..."
  → deploy
```

If the project is not in the map, the server returns `404` — the attacker cannot determine which project IDs exist.

### Constant-time HMAC comparison

HMAC comparison uses `hmac::verify()` — not a regular `==`. This prevents timing attacks where an attacker could guess the correct signature character by character based on response time differences.

### Replay attack mitigation

HMAC signs the request body but not a timestamp — Forgejo does not include one. A captured valid request could be resent by an attacker.

Practical impact is low: a replayed request would trigger a redundant deploy. The server would clone the repo, run a diff, find nothing changed, and upload nothing. No malicious content can be deployed this way.

Replay protection via in-memory signature cache is planned for Phase 2.

### Redacted config in logs

The `Config` struct implements a custom `Debug` trait that masks all sensitive fields. Sensitive values will never appear in logs or panic output:

```rust
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Config")
            .field("webhook_secret", &"[REDACTED]")
            .field("bunny_storage_password", &"[REDACTED]")
            .field("bunny_api_key", &"[REDACTED]")
            .finish()
    }
}
```

### No database

No credentials, tokens, or user data are stored on disk. The server is fully stateless. There is nothing to leak.

---

## Environment variables

### Global

| Variable | Required | Description |
|---|---|---|
| `BIND_ADDR` | yes | Address to listen on, e.g. `0.0.0.0:3000` |
| `BUNNY_API_KEY` | yes | Account API key — default for all projects, can be overridden per-project |

### Per-project

| Variable | Required | Description |
|---|---|---|
| `PROJECT_{ID}_REPO_URL` | yes | Full git clone URL |
| `PROJECT_{ID}_WEBHOOK_SECRET` | yes | HMAC secret — min 32 chars |
| `PROJECT_{ID}_BUNNY_STORAGE_ZONE` | yes | Storage zone name |
| `PROJECT_{ID}_BUNNY_STORAGE_PASSWORD` | yes | Storage zone password — upload/delete files |
| `PROJECT_{ID}_BUNNY_PULL_ZONE_ID` | yes | Pull zone ID — CDN cache purge |
| `PROJECT_{ID}_BUNNY_API_KEY` | no | Overrides global `BUNNY_API_KEY` for this project only |

`{ID}` is uppercase, e.g. `BLOG`, `DOCS`, `FILMLOG`.

### Credential resolution

Only `BUNNY_API_KEY` has a fallback — all other credentials are per-project required:

```
BUNNY_API_KEY:
  1. PROJECT_{ID}_BUNNY_API_KEY defined? → use it
  2. otherwise                           → use global BUNNY_API_KEY

BUNNY_STORAGE_ZONE:      per-project, required
BUNNY_STORAGE_PASSWORD:  per-project, required
BUNNY_PULL_ZONE_ID:      per-project, required (no fallback — wrong ID would purge wrong site)
```

---

## Example configurations

### Single project

```
BIND_ADDR=0.0.0.0:3000
BUNNY_API_KEY=global-api-key

PROJECT_FILMLOG_REPO_URL=https://codeberg.org/p1xel/filmlog.git
PROJECT_FILMLOG_WEBHOOK_SECRET=<random 32+ chars>
PROJECT_FILMLOG_BUNNY_STORAGE_ZONE=filmlogeu
PROJECT_FILMLOG_BUNNY_STORAGE_PASSWORD=xxx
PROJECT_FILMLOG_BUNNY_PULL_ZONE_ID=5651817
```

### Two projects — same Bunny account

```
BIND_ADDR=0.0.0.0:3000
BUNNY_API_KEY=global-api-key        ← set once, used by both projects

PROJECT_BLOG_REPO_URL=https://codeberg.org/user/blog.git
PROJECT_BLOG_WEBHOOK_SECRET=<random 32+ chars>
PROJECT_BLOG_BUNNY_STORAGE_ZONE=blogzone
PROJECT_BLOG_BUNNY_STORAGE_PASSWORD=xxx
PROJECT_BLOG_BUNNY_PULL_ZONE_ID=111111

PROJECT_DOCS_REPO_URL=https://codeberg.org/user/docs.git
PROJECT_DOCS_WEBHOOK_SECRET=<random 32+ chars>
PROJECT_DOCS_BUNNY_STORAGE_ZONE=docszone
PROJECT_DOCS_BUNNY_STORAGE_PASSWORD=yyy
PROJECT_DOCS_BUNNY_PULL_ZONE_ID=222222
```

### Two projects — different Bunny accounts

```
BIND_ADDR=0.0.0.0:3000
BUNNY_API_KEY=account-a-api-key     ← default

PROJECT_BLOG_REPO_URL=https://codeberg.org/user/blog.git
PROJECT_BLOG_WEBHOOK_SECRET=<random 32+ chars>
PROJECT_BLOG_BUNNY_STORAGE_ZONE=blogzone
PROJECT_BLOG_BUNNY_STORAGE_PASSWORD=xxx
PROJECT_BLOG_BUNNY_PULL_ZONE_ID=111111
                                    ← uses global BUNNY_API_KEY (account A)

PROJECT_DOCS_REPO_URL=https://codeberg.org/user/docs.git
PROJECT_DOCS_WEBHOOK_SECRET=<random 32+ chars>
PROJECT_DOCS_BUNNY_STORAGE_ZONE=docszone
PROJECT_DOCS_BUNNY_STORAGE_PASSWORD=yyy
PROJECT_DOCS_BUNNY_PULL_ZONE_ID=222222
PROJECT_DOCS_BUNNY_API_KEY=account-b-api-key
                                    ← overrides global (account B)
```

---

## Setup

```
1. Configure env vars for each project.

2. Add webhook in Forgejo for each project:
   Target URL:    https://xyz.b-cdn.net/hook/blog
   Secret:        <PROJECT_BLOG_WEBHOOK_SECRET>
   Trigger:       Push events
   Branch filter: pages
```

---

## Project structure

```
bunny-deploy/
├── src/
│   ├── providers/
│   │   ├── mod.rs          -- GitProvider trait + auto-detect by headers
│   │   ├── forgejo.rs      -- HMAC verify + push event parse (day 1)
│   │   ├── github.rs       -- stub
│   │   ├── gitea.rs        -- stub (near-identical to Forgejo)
│   │   └── gitlab.rs       -- stub
│   ├── bunny/
│   │   ├── storage.rs      -- file listing, upload, delete
│   │   └── cdn.rs          -- per-URL cache purge
│   ├── diff.rs             -- compute delta (upload / delete / skip), purge list
│   ├── webhook.rs          -- axum HTTP server, route handlers
│   ├── config.rs           -- Config + ProjectConfig structs, from_env(),
│   │                          validate(), redacted Debug, HashMap build
│   └── main.rs             -- load config → validate → pass to Axum State → start server
├── Dockerfile
├── docker-compose.example.yml
└── README.md
```

---

## Key Rust dependencies

| Crate | Purpose |
|---|---|
| `axum` | HTTP server |
| `tokio` | Async runtime |
| `reqwest` | HTTP client for Bunny API |
| `git2` | Git clone |
| `hmac` + `sha2` | Webhook signature verification (constant-time via `.verify()`) |
| `serde` + `serde_json` | JSON serialization |

---

## Deployment

### Bunny Magic Containers

```
BIND_ADDR=0.0.0.0:3000
BUNNY_API_KEY=xxx

PROJECT_FILMLOG_REPO_URL=https://codeberg.org/p1xel/filmlog.git
PROJECT_FILMLOG_WEBHOOK_SECRET=<random 32+ chars>
PROJECT_FILMLOG_BUNNY_STORAGE_ZONE=filmlogeu
PROJECT_FILMLOG_BUNNY_STORAGE_PASSWORD=xxx
PROJECT_FILMLOG_BUNNY_PULL_ZONE_ID=5651817
```

No persistent volume needed — the server is fully stateless.

Estimated cost at 30 webhooks/day: **~$0.27/month**.

### Docker Compose

```yaml
services:
  bunny-deploy:
    image: p1xel/bunny-deploy:latest
    ports:
      - "3000:3000"
    environment:
      BIND_ADDR: "0.0.0.0:3000"
      BUNNY_API_KEY: "xxx"
      PROJECT_FILMLOG_REPO_URL: "https://codeberg.org/p1xel/filmlog.git"
      PROJECT_FILMLOG_WEBHOOK_SECRET: "<random 32+ chars>"
      PROJECT_FILMLOG_BUNNY_STORAGE_ZONE: "filmlogeu"
      PROJECT_FILMLOG_BUNNY_STORAGE_PASSWORD: "xxx"
      PROJECT_FILMLOG_BUNNY_PULL_ZONE_ID: "5651817"
    restart: unless-stopped
```

### Docker CLI

```bash
docker run -d \
  -p 3000:3000 \
  -e BIND_ADDR=0.0.0.0:3000 \
  -e BUNNY_API_KEY=xxx \
  -e PROJECT_FILMLOG_REPO_URL=https://codeberg.org/p1xel/filmlog.git \
  -e PROJECT_FILMLOG_WEBHOOK_SECRET=random-32-plus-chars \
  -e PROJECT_FILMLOG_BUNNY_STORAGE_ZONE=filmlogeu \
  -e PROJECT_FILMLOG_BUNNY_STORAGE_PASSWORD=xxx \
  -e PROJECT_FILMLOG_BUNNY_PULL_ZONE_ID=5651817 \
  --restart unless-stopped \
  p1xel/bunny-deploy:latest
```

---

## Git provider support

Providers are pluggable via a Rust trait. Adding a new provider means implementing one file — no changes to core logic.

```rust
trait GitProvider {
    fn verify_signature(&self, payload: &[u8], headers: &HeaderMap) -> Result<()>;
    fn parse_push_event(&self, payload: &[u8]) -> Result<PushEvent>;
    fn name(&self) -> &'static str;
}
```

Provider is auto-detected from request headers:

| Header | Provider |
|---|---|
| `X-Forgejo-Event` | Forgejo |
| `X-Gitea-Event` | Gitea |
| `X-GitHub-Event` | GitHub |
| `X-Gitlab-Event` | GitLab |

| Provider | Status |
|---|---|
| Forgejo | Day 1 |
| Gitea | Stub (near-identical to Forgejo) |
| GitHub | Stub |
| GitLab | Stub |

---

## Phases

### Phase 1 — core
Forgejo webhook receiver, multi-project support, config loaded once at startup into `HashMap` via `std::env::vars()`, per-project Bunny credentials with `BUNNY_API_KEY` fallback, fail-fast startup validation, Bunny diff + sync, constant-time HMAC comparison, redacted config Debug, Docker image, README.

### Phase 2 — hardening
Replay protection via in-memory signature cache, retry logic on failed uploads, deploy queue (no concurrent deploys per project), structured JSON logs, `/health` endpoint.

### Phase 3 — providers
Fill in GitHub, Gitea, GitLab stubs.

---

## Out of scope (v1)

- Web dashboard
- Database or persistent storage
- DNS verification
- Build step (server only syncs pre-built output)
- Multi-region storage
- Rate limiting

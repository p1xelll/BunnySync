use axum::http::HeaderMap;

pub mod forgejo;
pub mod tangled;

pub trait GitProvider: Send + Sync {
    fn verify_signature(
        &self,
        payload: &[u8],
        headers: &HeaderMap,
        secret: &str,
    ) -> anyhow::Result<String>;
    fn parse_push_event(&self, payload: &[u8]) -> anyhow::Result<PushEvent>;
}

#[derive(Debug, Clone)]
pub struct PushEvent {
    pub ref_name: String,
    pub commit: String,
    pub is_test: bool, // true when before == after (test webhook)
}

pub fn detect_provider(headers: &HeaderMap) -> Option<Box<dyn GitProvider>> {
    // Detection order matters - check priority platforms first

    // Check for Forgejo first (Codeberg uses Forgejo - priority platform)
    if headers.contains_key("X-Forgejo-Event") {
        Some(Box::new(forgejo::ForgejoProvider))
    }
    // Check for Tangled (tangled.org - decentralized Git hosting on AT Protocol)
    else if headers.contains_key("X-Tangled-Event") {
        Some(Box::new(tangled::TangledProvider))
    }
    // No matching provider found
    else {
        None
    }
}

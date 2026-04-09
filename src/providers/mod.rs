use axum::http::HeaderMap;

pub mod forgejo;

pub trait GitProvider: Send + Sync {
    fn verify_signature(
        &self,
        payload: &[u8],
        headers: &HeaderMap,
        secret: &str,
    ) -> anyhow::Result<String>;
    fn parse_push_event(&self, payload: &[u8]) -> anyhow::Result<PushEvent>;
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
}

#[derive(Debug, Clone)]
pub struct PushEvent {
    pub ref_name: String,
    pub commit: String,
}

pub fn detect_provider(headers: &HeaderMap) -> Option<Box<dyn GitProvider>> {
    if headers.contains_key("X-Forgejo-Event") || headers.contains_key("X-Gitea-Event") {
        Some(Box::new(forgejo::ForgejoProvider))
    } else {
        None
    }
}

use super::{GitProvider, PushEvent};
use anyhow::{anyhow, Context, Result};
use axum::http::HeaderMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub struct ForgejoProvider;

type HmacSha256 = Hmac<Sha256>;

impl GitProvider for ForgejoProvider {
    fn verify_signature(
        &self,
        payload: &[u8],
        headers: &HeaderMap,
        secret: &str,
    ) -> Result<String> {
        let signature = headers
            .get("X-Forgejo-Signature")
            .or_else(|| headers.get("X-Gitea-Signature"))
            .ok_or_else(|| anyhow!("missing signature header"))?
            .to_str()
            .context("invalid signature header")?;

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| anyhow!("invalid secret: {}", e))?;
        mac.update(payload);

        let expected = hex::decode(signature).context("invalid signature hex")?;

        mac.verify_slice(&expected)
            .map_err(|_| anyhow!("signature verification failed"))?;

        Ok(signature.to_string())
    }

    fn parse_push_event(&self, payload: &[u8]) -> Result<PushEvent> {
        let json: serde_json::Value = serde_json::from_slice(payload).context("invalid JSON")?;

        let ref_name = json
            .get("ref")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing ref"))?
            .to_string();

        let commit = json
            .get("after")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        Ok(PushEvent { ref_name, commit })
    }

    fn name(&self) -> &'static str {
        "Forgejo"
    }
}

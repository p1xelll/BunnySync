//! GitHub webhook provider
//!
//! GitHub is the world's largest Git hosting platform.
//! Webhook documentation: https://docs.github.com/en/webhooks
//!
//! ## Key characteristics:
//! - Event header: `X-GitHub-Event` (e.g., "push", "ping")
//! - Signature header: `X-Hub-Signature-256` with format `sha256=<hex>`
//! - Algorithm: HMAC-SHA256
//! - Delivery ID: `X-GitHub-Delivery` (UUID)
//! - User-Agent: `GitHub-Hookshot/<id>`

use super::{GitProvider, PushEvent};
use anyhow::{Context, Result, anyhow};
use axum::http::HeaderMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// GitHub webhook provider
///
/// Implements webhook signature verification and payload parsing
/// for the GitHub Git hosting platform.
pub struct GithubProvider;

type HmacSha256 = Hmac<Sha256>;

impl GitProvider for GithubProvider {
    /// Verify webhook signature using HMAC-SHA256
    ///
    /// # Arguments
    /// * `payload` - Raw request body bytes
    /// * `headers` - HTTP headers from the request
    /// * `secret` - Webhook secret configured in GitHub
    ///
    /// # Returns
    /// * `Ok(String)` - The signature value for deduplication cache
    /// * `Err` - If signature is missing, malformed, or invalid
    ///
    /// # Signature Format
    /// GitHub sends signatures in the format: `sha256=<hex>` in the
    /// `X-Hub-Signature-256` header. We strip the `sha256=` prefix before verification.
    fn verify_signature(
        &self,
        payload: &[u8],
        headers: &HeaderMap,
        secret: &str,
    ) -> Result<String> {
        let signature_header = headers
            .get("X-Hub-Signature-256")
            .ok_or_else(|| anyhow!("missing X-Hub-Signature-256 header"))?
            .to_str()
            .context("invalid signature header encoding")?;

        // Parse signature format: sha256=<hex>
        let signature_value = signature_header
            .strip_prefix("sha256=")
            .ok_or_else(|| anyhow!("invalid signature format: expected sha256=<hex>"))?;

        // Decode hex signature
        let signature_bytes =
            hex::decode(signature_value).context("invalid signature hex encoding")?;

        // Compute expected signature using HMAC-SHA256
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| anyhow!("invalid secret: {}", e))?;
        mac.update(payload);

        // Constant-time comparison to prevent timing attacks
        mac.verify_slice(&signature_bytes)
            .map_err(|_| anyhow!("signature verification failed"))?;

        Ok(signature_value.to_string())
    }

    /// Parse push event from webhook payload
    ///
    /// # Arguments
    /// * `payload` - Raw JSON payload bytes
    ///
    /// # Returns
    /// * `Ok(PushEvent)` - Parsed push event with ref, commit, and test detection
    /// * `Err` - If payload is invalid JSON or missing required fields
    ///
    /// # Payload Structure
    /// GitHub push events include:
    /// - `ref`: Git reference (e.g., "refs/heads/main")
    /// - `before`: Commit SHA before push
    /// - `after`: Commit SHA after push (the new commit)
    fn parse_push_event(&self, payload: &[u8]) -> Result<PushEvent> {
        let json: serde_json::Value =
            serde_json::from_slice(payload).context("invalid JSON payload")?;

        // Check for ping event (GitHub sends this when testing webhooks)
        // Ping events have hook_id but no ref field
        if json.get("hook_id").is_some() && json.get("ref").is_none() {
            return Ok(PushEvent {
                ref_name: String::new(),
                commit: "ping".to_string(),
                is_test: true,
            });
        }

        // Extract git reference (e.g., refs/heads/main)
        let ref_name = json
            .get("ref")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'ref' field in payload"))?
            .to_string();

        // Extract commit SHA before push (for test detection)
        let before = json.get("before").and_then(|v| v.as_str()).unwrap_or("");

        // Extract commit SHA after push (the new commit)
        let after = json
            .get("after")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'after' field in payload"))?;

        // Detect test webhook: before and after are the same and not empty
        let is_test = !before.is_empty() && before == after;

        Ok(PushEvent {
            ref_name,
            commit: after.to_string(),
            is_test,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn create_test_headers(signature: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("X-GitHub-Event", HeaderValue::from_static("push"));
        headers.insert(
            "X-Hub-Signature-256",
            HeaderValue::from_str(&format!("sha256={}", signature)).unwrap(),
        );
        headers.insert(
            "X-GitHub-Delivery",
            HeaderValue::from_static("test-delivery-id"),
        );
        headers
    }

    #[test]
    fn test_verify_signature_valid() {
        let provider = GithubProvider;
        let secret = "test-secret-min-32-characters-long";
        let payload = br#"{"ref":"refs/heads/main","before":"abc123","after":"def456"}"#;

        // Compute expected signature
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let signature = hex::encode(mac.finalize().into_bytes());

        let headers = create_test_headers(&signature);

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), signature);
    }

    #[test]
    fn test_verify_signature_invalid() {
        let provider = GithubProvider;
        let secret = "test-secret-min-32-characters-long";
        let payload = br#"{"ref":"refs/heads/main"}"#;

        let headers = create_test_headers("invalidsignature");

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_missing_header() {
        let provider = GithubProvider;
        let secret = "test-secret";
        let payload = br#"{}"#;

        let headers = HeaderMap::new();

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("X-Hub-Signature-256")
        );
    }

    #[test]
    fn test_verify_signature_wrong_prefix() {
        let provider = GithubProvider;
        let secret = "test-secret";
        let payload = br#"{}"#;

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Hub-Signature-256",
            HeaderValue::from_static("invalid-prefix-abc123"),
        );

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sha256="));
    }

    #[test]
    fn test_parse_push_event_valid() {
        let provider = GithubProvider;
        let payload = br#"
            {
                "ref": "refs/heads/main",
                "before": "28e1879d029cb852e4844d9c718537df08844e03",
                "after": "bffeb74224043ba2feb48d137756c8a9331c449a",
                "pusher": {"name": "octocat", "email": "octocat@github.com"},
                "repository": {"clone_url": "https://github.com/octocat/Hello-World.git"}
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert_eq!(event.ref_name, "refs/heads/main");
        assert_eq!(event.commit, "bffeb74224043ba2feb48d137756c8a9331c449a");
        assert!(!event.is_test);
    }

    #[test]
    fn test_parse_push_event_test_webhook() {
        let provider = GithubProvider;
        let payload = br#"
            {
                "ref": "refs/heads/main",
                "before": "abc123",
                "after": "abc123"
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert!(event.is_test);
    }

    #[test]
    fn test_parse_push_event_ping_event() {
        // GitHub ping events have hook_id but no ref/before/after fields
        // Should be treated as test webhook (returns is_test=true)
        let provider = GithubProvider;
        let payload = br#"
            {
                "zen": "Non-blocking is better than blocking.",
                "hook_id": 605893990,
                "hook": {"type": "Repository", "id": 605893990}
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert!(event.is_test);
        assert_eq!(event.commit, "ping");
    }

    #[test]
    fn test_parse_push_event_missing_ref() {
        let provider = GithubProvider;
        let payload = br#"{"after": "abc123"}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ref"));
    }

    #[test]
    fn test_parse_push_event_missing_after() {
        let provider = GithubProvider;
        let payload = br#"{"ref": "refs/heads/main"}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("after"));
    }
}

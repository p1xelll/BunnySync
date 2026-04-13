//! GitLab webhook provider
//!
//! GitLab is a complete DevOps platform.
//! Webhook documentation: https://docs.gitlab.com/user/project/integrations/webhooks/
//!
//! ## Key characteristics:
//! - Event header: `X-Gitlab-Event` (e.g., "Push Hook", "Tag Push Hook")
//! - Token header: `X-Gitlab-Token` for secret token verification
//! - Signature header: `X-Gitlab-Signature` with hex-encoded HMAC-SHA256 (optional, for advanced verification)
//! - Algorithm: HMAC-SHA256 (when signature is used)
//! - Signature format: Raw hex string (no prefix)

use super::{GitProvider, PushEvent};
use anyhow::{Context, Result, anyhow};
use axum::http::HeaderMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

/// GitLab webhook provider
///
/// Implements webhook signature verification and payload parsing
/// for the GitLab Git hosting platform.
pub struct GitlabProvider;

type HmacSha256 = Hmac<Sha256>;

impl GitProvider for GitlabProvider {
    /// Verify webhook signature using HMAC-SHA256
    ///
    /// # Arguments
    /// * `payload` - Raw request body bytes
    /// * `headers` - HTTP headers from the request
    /// * `secret` - Webhook secret configured in GitLab
    ///
    /// # Returns
    /// * `Ok(String)` - The signature value for deduplication cache
    /// * `Err` - If signature is missing, malformed, or invalid
    ///
    /// # Signature Format
    /// GitLab sends the secret token in the `X-Gitlab-Token` header.
    /// For HMAC-SHA256 signature verification, the `X-Gitlab-Signature` header
    /// contains the raw hex-encoded signature.
    fn verify_signature(
        &self,
        payload: &[u8],
        headers: &HeaderMap,
        secret: &str,
    ) -> Result<String> {
        // Get idempotency key first - GitLab sends a unique key per webhook request
        // This is the proper deduplication key (not the static token)
        let idempotency_key = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok());

        // GitLab sends secret token in X-Gitlab-Token header
        // First check for simple token verification
        if let Some(token_header) = headers.get("X-Gitlab-Token") {
            let token = token_header
                .to_str()
                .context("invalid token header encoding")?;

            if token == secret {
                // Use idempotency key if available, otherwise fall back to a hash of payload+token
                // The token alone is NOT suitable for deduplication as it's static per project
                let cache_key = if let Some(key) = idempotency_key {
                    key.to_string()
                } else {
                    // Fallback: hash the payload with the token to create a unique signature
                    let mut hasher = sha2::Sha256::new();
                    sha2::Digest::update(&mut hasher, payload);
                    sha2::Digest::update(&mut hasher, token.as_bytes());
                    format!("token:{}", hex::encode(sha2::Digest::finalize(hasher)))
                };
                return Ok(cache_key);
            }
        }

        // If token doesn't match, check for HMAC-SHA256 signature in X-Gitlab-Signature header
        if let Some(signature_header) = headers.get("X-Gitlab-Signature") {
            let signature = signature_header
                .to_str()
                .context("invalid signature header encoding")?;

            // Compute expected signature using HMAC-SHA256
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                .map_err(|e| anyhow!("invalid secret: {}", e))?;
            mac.update(payload);

            // Decode the hex signature from the header
            let expected = hex::decode(signature).context("invalid signature hex encoding")?;

            // Constant-time comparison to prevent timing attacks
            mac.verify_slice(&expected)
                .map_err(|_| anyhow!("signature verification failed"))?;

            // Use idempotency key if available, otherwise use the HMAC signature
            // (HMAC signature is unique per payload, so it's safe for deduplication)
            let cache_key = if let Some(key) = idempotency_key {
                key.to_string()
            } else {
                signature.to_string()
            };
            return Ok(cache_key);
        }

        // Neither token nor signature header found
        Err(anyhow!(
            "missing X-Gitlab-Token or X-Gitlab-Signature header"
        ))
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
    /// GitLab push events include:
    /// - `ref`: Git reference (e.g., "refs/heads/main")
    /// - `before`: Commit SHA before push
    /// - `after`: Commit SHA after push (the new commit)
    /// - `checkout_sha`: The commit SHA for the updated reference
    fn parse_push_event(&self, payload: &[u8]) -> Result<PushEvent> {
        let json: serde_json::Value =
            serde_json::from_slice(payload).context("invalid JSON payload")?;

        // Extract git reference (e.g., refs/heads/main)
        let ref_name = json
            .get("ref")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'ref' field in payload"))?
            .to_string();

        // Extract commit SHA before push (for test detection)
        let before = json.get("before").and_then(|v| v.as_str()).unwrap_or("");

        // Extract commit SHA after push (the new commit)
        // GitLab uses "checkout_sha" or "after" depending on event type
        let after = json
            .get("after")
            .and_then(|v| v.as_str())
            .or_else(|| json.get("checkout_sha").and_then(|v| v.as_str()))
            .ok_or_else(|| anyhow!("missing 'after' or 'checkout_sha' field in payload"))?;

        // Detect test webhook: before and after are the same and not empty
        // GitLab sends test webhooks with before == after (all zeros for new branches)
        let is_test = before == after
            && !before.is_empty()
            && before != "0000000000000000000000000000000000000000";

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

    fn create_test_headers_with_token(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Event", HeaderValue::from_static("Push Hook"));
        headers.insert("X-Gitlab-Token", HeaderValue::from_str(token).unwrap());
        headers
    }

    fn create_test_headers_with_signature(signature: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("X-Gitlab-Event", HeaderValue::from_static("Push Hook"));
        headers.insert(
            "X-Gitlab-Signature",
            HeaderValue::from_str(signature).unwrap(),
        );
        headers
    }

    #[test]
    fn test_verify_signature_with_token_valid() {
        let provider = GitlabProvider;
        let secret = "my-webhook-secret";
        let payload = br#"{"ref":"refs/heads/main","before":"abc123","after":"def456"}"#;

        let mut headers = create_test_headers_with_token(secret);
        headers.insert("Idempotency-Key", HeaderValue::from_static("test-key-123"));

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_ok());
        // Should use idempotency key when available
        assert_eq!(result.unwrap(), "test-key-123");
    }

    #[test]
    fn test_verify_signature_with_token_valid_no_idempotency_key() {
        let provider = GitlabProvider;
        let secret = "my-webhook-secret";
        let payload = br#"{"ref":"refs/heads/main","before":"abc123","after":"def456"}"#;

        let headers = create_test_headers_with_token(secret);

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_ok());
        // Should return a hashed signature starting with "token:" when no idempotency key
        let sig = result.unwrap();
        assert!(sig.starts_with("token:"));
        assert_eq!(sig.len(), 70); // "token:" (6) + 64 hex chars
    }

    #[test]
    fn test_verify_signature_with_token_invalid() {
        let provider = GitlabProvider;
        let secret = "my-webhook-secret";
        let payload = br#"{"ref":"refs/heads/main"}"#;

        let headers = create_test_headers_with_token("wrong-secret");

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_with_hmac_valid() {
        let provider = GitlabProvider;
        let secret = "test-secret-min-32-characters-long";
        let payload = br#"{"ref":"refs/heads/main","before":"abc123","after":"def456"}"#;

        // Compute expected signature
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let signature = hex::encode(mac.finalize().into_bytes());

        let mut headers = create_test_headers_with_signature(&signature);
        headers.insert("Idempotency-Key", HeaderValue::from_static("hmac-key-456"));

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_ok());
        // Should use idempotency key when available
        assert_eq!(result.unwrap(), "hmac-key-456");
    }

    #[test]
    fn test_verify_signature_with_hmac_valid_no_idempotency_key() {
        let provider = GitlabProvider;
        let secret = "test-secret-min-32-characters-long";
        let payload = br#"{"ref":"refs/heads/main","before":"abc123","after":"def456"}"#;

        // Compute expected signature
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let signature = hex::encode(mac.finalize().into_bytes());

        let headers = create_test_headers_with_signature(&signature);

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_ok());
        // Should use HMAC signature when no idempotency key
        assert_eq!(result.unwrap(), signature);
    }

    #[test]
    fn test_verify_signature_with_hmac_invalid() {
        let provider = GitlabProvider;
        let secret = "test-secret-min-32-characters-long";
        let payload = br#"{"ref":"refs/heads/main"}"#;

        let headers = create_test_headers_with_signature("invalidsignature");

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_missing_header() {
        let provider = GitlabProvider;
        let secret = "test-secret";
        let payload = br#"{}"#;

        let headers = HeaderMap::new();

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn test_parse_push_event_valid() {
        let provider = GitlabProvider;
        let payload = br#"
            {
                "object_kind": "push",
                "event_name": "push",
                "before": "95790bf891e76fee5e1747ab589903a6a1f80f22",
                "after": "da1560886d4f094c3e6c9ef40349f7d38b5d27d7",
                "ref": "refs/heads/master",
                "user_name": "John Smith",
                "user_username": "jsmith",
                "user_email": "john@example.com",
                "project_id": 15,
                "project": {
                    "name": "Diaspora",
                    "web_url": "http://example.com/mike/diaspora"
                },
                "commits": [
                    {
                        "id": "da1560886d4f094c3e6c9ef40349f7d38b5d27d7",
                        "message": "fixed readme"
                    }
                ]
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert_eq!(event.ref_name, "refs/heads/master");
        assert_eq!(event.commit, "da1560886d4f094c3e6c9ef40349f7d38b5d27d7");
        assert!(!event.is_test);
    }

    #[test]
    fn test_parse_push_event_with_checkout_sha() {
        let provider = GitlabProvider;
        let payload = br#"
            {
                "object_kind": "push",
                "ref": "refs/heads/develop",
                "checkout_sha": "abc123def456789"
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert_eq!(event.ref_name, "refs/heads/develop");
        assert_eq!(event.commit, "abc123def456789");
    }

    #[test]
    fn test_parse_push_event_test_webhook() {
        let provider = GitlabProvider;
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
    fn test_parse_push_event_new_branch() {
        // New branch pushes have before = 0000... which should not be treated as test
        let provider = GitlabProvider;
        let payload = br#"
            {
                "ref": "refs/heads/feature-branch",
                "before": "0000000000000000000000000000000000000000",
                "after": "da1560886d4f094c3e6c9ef40349f7d38b5d27d7"
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert_eq!(event.ref_name, "refs/heads/feature-branch");
        assert!(!event.is_test);
    }

    #[test]
    fn test_parse_push_event_missing_ref() {
        let provider = GitlabProvider;
        let payload = br#"{"after": "abc123"}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ref"));
    }

    #[test]
    fn test_parse_push_event_missing_after() {
        let provider = GitlabProvider;
        let payload = br#"{"ref": "refs/heads/main"}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("after") || err_msg.contains("checkout_sha"));
    }
}

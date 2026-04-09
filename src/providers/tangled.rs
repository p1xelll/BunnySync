//! Tangled (tangled.org) webhook provider
//!
//! Tangled is a decentralized Git hosting platform using AT Protocol.
//! Webhook documentation: https://docs.tangled.org/webhooks.html
//!
//! ## Key characteristics:
//! - Event header: `X-Tangled-Event` (e.g., "push")
//! - Signature header: `X-Tangled-Signature-256` with format `sha256=<hex>`
//! - Algorithm: HMAC-SHA256 (same as Forgejo, different header format)
//! - Delivery ID: `X-Tangled-Delivery` (UUID)
//! - Uses DIDs (Decentralized Identifiers) for user identification

use super::{GitProvider, PushEvent};
use anyhow::{Context, Result, anyhow};
use axum::http::HeaderMap;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Tangled webhook provider
///
/// Implements webhook signature verification and payload parsing
/// for the Tangled Git hosting platform.
pub struct TangledProvider;

type HmacSha256 = Hmac<Sha256>;

impl GitProvider for TangledProvider {
    /// Verify webhook signature using HMAC-SHA256
    ///
    /// # Arguments
    /// * `payload` - Raw request body bytes
    /// * `headers` - HTTP headers from the request
    /// * `secret` - Webhook secret configured in Tangled
    ///
    /// # Returns
    /// * `Ok(String)` - The signature value for deduplication cache
    /// * `Err` - If signature is missing, malformed, or invalid
    ///
    /// # Signature Format
    /// Tangled sends signatures in the format: `sha256=<hex>`
    /// We strip the `sha256=` prefix and compare the hex value.
    fn verify_signature(
        &self,
        payload: &[u8],
        headers: &HeaderMap,
        secret: &str,
    ) -> Result<String> {
        // Extract X-Tangled-Signature-256 header
        let signature_header = headers
            .get("X-Tangled-Signature-256")
            .ok_or_else(|| anyhow!("missing X-Tangled-Signature-256 header"))?
            .to_str()
            .context("invalid signature header encoding")?;

        // Parse signature format: sha256=<hex>
        // The prefix must be stripped before verification
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

        // Return signature value for deduplication cache
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
    /// Tangled push events include:
    /// - `ref`: Git reference (e.g., "refs/heads/main")
    /// - `before`: Commit SHA before push
    /// - `after`: Commit SHA after push (the new commit)
    /// - `pusher.did`: DID of the user who pushed
    /// - `repository.clone_url`: URL for cloning the repository
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
        let after = json
            .get("after")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'after' field in payload"))?;

        // Detect test webhook: before and after are the same and not empty
        // This happens when testing the webhook in Tangled UI
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
        headers.insert("X-Tangled-Event", HeaderValue::from_static("push"));
        headers.insert(
            "X-Tangled-Signature-256",
            HeaderValue::from_str(&format!("sha256={}", signature)).unwrap(),
        );
        headers.insert(
            "X-Tangled-Delivery",
            HeaderValue::from_static("test-delivery-id"),
        );
        headers
    }

    #[test]
    fn test_verify_signature_valid() {
        let provider = TangledProvider;
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
        let provider = TangledProvider;
        let secret = "test-secret-min-32-characters-long";
        let payload = br#"{"ref":"refs/heads/main"}"#;

        let headers = create_test_headers("invalidsignature");

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_missing_header() {
        let provider = TangledProvider;
        let secret = "test-secret";
        let payload = br#"{}"#;

        let headers = HeaderMap::new();

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn test_verify_signature_wrong_prefix() {
        let provider = TangledProvider;
        let secret = "test-secret";
        let payload = br#"{}"#;

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Tangled-Signature-256",
            HeaderValue::from_static("invalid-prefix-abc123"),
        );

        let result = provider.verify_signature(payload, &headers, secret);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("sha256="));
    }

    #[test]
    fn test_parse_push_event_valid() {
        let provider = TangledProvider;
        let payload = br#"
            {
                "ref": "refs/heads/main",
                "before": "c04ddf64eddc90e4e2a9846ba3b43e67a0e2865e",
                "after": "7b320e5cbee2734071e4310c1d9ae401d8f6cab5",
                "pusher": {"did": "did:plc:hwevmowznbiukdf6uk5dwrrq"},
                "repository": {"clone_url": "https://tangled.org/did:plc:test/repo"}
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert_eq!(event.ref_name, "refs/heads/main");
        assert_eq!(event.commit, "7b320e5cbee2734071e4310c1d9ae401d8f6cab5");
        assert!(!event.is_test);
    }

    #[test]
    fn test_parse_push_event_test_webhook() {
        let provider = TangledProvider;
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
    fn test_parse_push_event_missing_ref() {
        let provider = TangledProvider;
        let payload = br#"{"after": "abc123"}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ref"));
    }

    #[test]
    fn test_parse_push_event_missing_after() {
        let provider = TangledProvider;
        let payload = br#"{"ref": "refs/heads/main"}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("after"));
    }
}

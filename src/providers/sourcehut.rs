//! SourceHut (git.sr.ht) webhook provider
//!
//! SourceHut is a free-software forge emphasizing simplicity and open standards.
//! Webhook documentation: https://man.sr.ht/graphql.md#webhooks
//! GraphQL schema: https://git.sr.ht/~sircmpwn/git.sr.ht/tree/master/item/api/graph/schema.graphqls
//!
//! ## Key characteristics:
//! - Webhooks are configured via GraphQL mutations (`createGitWebhook`)
//! - Event type: `GIT_POST_RECEIVE`
//! - Signature header: `X-Payload-Signature` (base64-encoded Ed25519 signature)
//! - Nonce header: `X-Payload-Nonce` (used as part of the signed message)
//! - Algorithm: Ed25519 (asymmetric signature verification)
//! - Public key: available at `<service>/query/api-meta.json`
//! - Payload format: GraphQL query result (JSON with `data.webhook` structure)
//!
//! ## Webhook payload structure:
//! SourceHut webhooks use a GraphQL-native approach. The user defines a GraphQL
//! query when creating the webhook subscription. When an event occurs, SourceHut
//! executes the query and POSTs the result as JSON to the configured URL.
//!
//! The recommended GraphQL query for detecting git pushes is:
//!
//! ```graphql
//! query {
//!   webhook {
//!     uuid
//!     event
//!     date
//!     ... on GitEvent {
//!       updates {
//!         ref { name }
//!         old { id }
//!         new { id }
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! This produces a payload with the following structure:
//! ```json
//! {
//!   "data": {
//!     "webhook": {
//!       "uuid": "...",
//!       "event": "GIT_POST_RECEIVE",
//!       "date": "2024-01-01T00:00:00Z",
//!       "updates": [
//!         {
//!           "ref": { "name": "refs/heads/main" },
//!           "old": { "id": "abc123..." },
//!           "new": { "id": "def456..." }
//!         }
//!       ]
//!     }
//!   }
//! }
//! ```

use super::{GitProvider, PushEvent};
use anyhow::{Context, Result, anyhow};
use axum::http::HeaderMap;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

const SOURCEHUT_PUBLIC_KEY_B64: &str = "uX7KWyyDNMaBma4aVbJ/cbUQpdjqczuCyK/HxzV/u+4=";

/// SourceHut webhook provider
///
/// Implements webhook signature verification and payload parsing
/// for the SourceHut Git hosting platform (git.sr.ht).
///
/// Unlike other providers that use HMAC-SHA256 with a shared secret,
/// SourceHut uses Ed25519 asymmetric signatures. The public key is
/// well-known and shared across all SourceHut instances.
pub struct SourcehutProvider;

impl SourcehutProvider {
    fn resolve_verifying_key() -> Result<VerifyingKey> {
        let public_key_bytes = base64::engine::general_purpose::STANDARD
            .decode(SOURCEHUT_PUBLIC_KEY_B64)
            .context("invalid SourceHut public key")?;

        VerifyingKey::from_bytes(
            public_key_bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("invalid public key length"))?,
        )
        .map_err(|e| anyhow!("invalid SourceHut public key: {}", e))
    }
}

impl GitProvider for SourcehutProvider {
    /// Verify webhook signature using Ed25519
    ///
    /// # Arguments
    /// * `payload` - Raw request body bytes
    /// * `headers` - HTTP headers from the request
    /// * `secret` - Unused (SourceHut uses a well-known public key for verification)
    ///
    /// # Returns
    /// * `Ok(String)` - The signature value for deduplication cache
    /// * `Err` - If signature is missing, malformed, or invalid
    ///
    /// # Signature Format
    /// SourceHut sends an Ed25519 signature in the `X-Payload-Signature` header
    /// (base64-encoded). The signed message is the concatenation of the request
    /// body and the `X-Payload-Nonce` header value.
    ///
    /// The `secret` parameter is not used because SourceHut uses asymmetric
    /// cryptography with a well-known public key rather than a shared secret.
    fn verify_signature(
        &self,
        payload: &[u8],
        headers: &HeaderMap,
        _secret: &str,
    ) -> Result<String> {
        let signature_b64 = headers
            .get("X-Payload-Signature")
            .ok_or_else(|| anyhow!("missing X-Payload-Signature header"))?
            .to_str()
            .context("invalid signature header encoding")?;

        let nonce = headers
            .get("X-Payload-Nonce")
            .ok_or_else(|| anyhow!("missing X-Payload-Nonce header"))?
            .to_str()
            .context("invalid nonce header encoding")?;

        let signature_bytes = base64::engine::general_purpose::STANDARD
            .decode(signature_b64)
            .context("invalid signature base64 encoding")?;

        let signature = Signature::try_from(signature_bytes.as_slice())
            .map_err(|e| anyhow!("invalid Ed25519 signature: {}", e))?;

        let verifying_key = Self::resolve_verifying_key()?;

        let mut message = payload.to_vec();
        message.extend_from_slice(nonce.as_bytes());

        verifying_key
            .verify(&message, &signature)
            .map_err(|e| anyhow!("signature verification failed: {}", e))?;

        Ok(signature_b64.to_string())
    }

    /// Parse push event from webhook payload
    ///
    /// # Arguments
    /// * `payload` - Raw JSON payload bytes (GraphQL query result)
    ///
    /// # Returns
    /// * `Ok(PushEvent)` - Parsed push event with ref, commit, and test detection
    /// * `Err` - If payload is invalid JSON or missing required fields
    ///
    /// # Payload Structure
    /// SourceHut webhooks use a GraphQL-native format. The payload is the result
    /// of the user-defined GraphQL query, nested under `data.webhook`.
    ///
    /// For git push events (`GIT_POST_RECEIVE`), the `updates` array contains
    /// information about each updated reference, including the ref name and
    /// old/new commit IDs.
    fn parse_push_event(&self, payload: &[u8]) -> Result<PushEvent> {
        let json: serde_json::Value =
            serde_json::from_slice(payload).context("invalid JSON payload")?;

        let webhook = json
            .get("data")
            .and_then(|d| d.get("webhook"))
            .ok_or_else(|| anyhow!("missing 'data.webhook' in payload"))?;

        let updates = webhook
            .get("updates")
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow!("missing 'updates' array in webhook payload"))?;

        if updates.is_empty() {
            return Err(anyhow!("empty updates array in webhook payload"));
        }

        let first_update = &updates[0];

        let ref_name = first_update
            .pointer("/ref/name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'ref.name' field in update"))?
            .to_string();

        let old_id = first_update
            .pointer("/old/id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let new_id = first_update
            .pointer("/new/id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'new.id' field in update"))?;

        let is_test = !old_id.is_empty() && old_id == new_id;

        Ok(PushEvent {
            ref_name,
            commit: new_id.to_string(),
            is_test,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use ed25519_dalek::{Signer, SigningKey};

    const TEST_SIGNING_KEY_SEED: [u8; 32] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32,
        0x10, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
        0x32, 0x10,
    ];

    fn sign_payload(payload: &[u8], nonce: &str) -> (String, HeaderMap) {
        let signing_key = SigningKey::from_bytes(&TEST_SIGNING_KEY_SEED);

        let mut message = payload.to_vec();
        message.extend_from_slice(nonce.as_bytes());

        let signature = signing_key.sign(&message);
        let signature_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Payload-Signature",
            HeaderValue::from_str(&signature_b64).unwrap(),
        );
        headers.insert("X-Payload-Nonce", HeaderValue::from_str(nonce).unwrap());

        (signature_b64, headers)
    }

    fn verify_with_key(
        payload: &[u8],
        headers: &HeaderMap,
        verifying_key: &VerifyingKey,
    ) -> Result<String> {
        let signature_b64 = headers
            .get("X-Payload-Signature")
            .unwrap()
            .to_str()
            .unwrap();

        let nonce = headers.get("X-Payload-Nonce").unwrap().to_str().unwrap();

        let signature_bytes = base64::engine::general_purpose::STANDARD
            .decode(signature_b64)
            .unwrap();

        let signature = Signature::try_from(signature_bytes.as_slice()).unwrap();

        let mut message = payload.to_vec();
        message.extend_from_slice(nonce.as_bytes());

        verifying_key
            .verify(&message, &signature)
            .map_err(|e| anyhow!("signature verification failed: {}", e))?;

        Ok(signature_b64.to_string())
    }

    #[test]
    fn test_verify_signature_valid() {
        let signing_key = SigningKey::from_bytes(&TEST_SIGNING_KEY_SEED);
        let test_verifying_key = signing_key.verifying_key();

        let payload = br#"{"data":{"webhook":{"uuid":"test","event":"GIT_POST_RECEIVE","updates":[{"ref":{"name":"refs/heads/main"},"old":{"id":"abc123"},"new":{"id":"def456"}}]}}}"#;
        let (_, headers) = sign_payload(payload, "test-nonce-12345");

        let result = verify_with_key(payload, &headers, &test_verifying_key);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_signature_invalid() {
        let provider = SourcehutProvider;
        let payload = br#"{"data":{"webhook":{}}}"#;

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Payload-Signature",
            HeaderValue::from_static("aW52YWxpZHNpZ25hdHVyZQ=="),
        );
        headers.insert("X-Payload-Nonce", HeaderValue::from_static("test-nonce"));

        let result = provider.verify_signature(payload, &headers, "unused-secret");
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_missing_signature_header() {
        let provider = SourcehutProvider;
        let payload = br#"{}"#;

        let mut headers = HeaderMap::new();
        headers.insert("X-Payload-Nonce", HeaderValue::from_static("test-nonce"));

        let result = provider.verify_signature(payload, &headers, "unused-secret");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("X-Payload-Signature")
        );
    }

    #[test]
    fn test_verify_signature_missing_nonce_header() {
        let provider = SourcehutProvider;
        let payload = br#"{}"#;

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Payload-Signature",
            HeaderValue::from_static("aW52YWxpZA=="),
        );

        let result = provider.verify_signature(payload, &headers, "unused-secret");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("X-Payload-Nonce"));
    }

    #[test]
    fn test_verify_signature_wrong_key() {
        let provider = SourcehutProvider;
        let payload = br#"{"data":{"webhook":{}}}"#;

        let (_, headers) = sign_payload(payload, "test-nonce");

        // Uses TEST key but provider verifies against real sr.ht public key
        let result = provider.verify_signature(payload, &headers, "unused-secret");
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_signature_tampered_payload() {
        let signing_key = SigningKey::from_bytes(&TEST_SIGNING_KEY_SEED);
        let test_verifying_key = signing_key.verifying_key();

        let original_payload = br#"{"data":{"webhook":{"uuid":"test"}}}"#;
        let tampered_payload = br#"{"data":{"webhook":{"uuid":"tampered"}}}"#;

        let (_, headers) = sign_payload(original_payload, "test-nonce");

        let result = verify_with_key(tampered_payload, &headers, &test_verifying_key);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_push_event_valid() {
        let provider = SourcehutProvider;
        let payload = br#"
            {
                "data": {
                    "webhook": {
                        "uuid": "550e8400-e29b-41d4-a716-446655440000",
                        "event": "GIT_POST_RECEIVE",
                        "date": "2024-01-15T10:30:00Z",
                        "updates": [
                            {
                                "ref": { "name": "refs/heads/main" },
                                "old": { "id": "28e1879d029cb852e4844d9c718537df08844e03" },
                                "new": { "id": "bffeb74224043ba2feb48d137756c8a9331c449a" }
                            }
                        ]
                    }
                }
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert_eq!(event.ref_name, "refs/heads/main");
        assert_eq!(event.commit, "bffeb74224043ba2feb48d137756c8a9331c449a");
        assert!(!event.is_test);
    }

    #[test]
    fn test_parse_push_event_test_webhook() {
        let provider = SourcehutProvider;
        let payload = br#"
            {
                "data": {
                    "webhook": {
                        "uuid": "test-uuid",
                        "event": "GIT_POST_RECEIVE",
                        "date": "2024-01-15T10:30:00Z",
                        "updates": [
                            {
                                "ref": { "name": "refs/heads/main" },
                                "old": { "id": "abc123" },
                                "new": { "id": "abc123" }
                            }
                        ]
                    }
                }
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert!(event.is_test);
    }

    #[test]
    fn test_parse_push_event_missing_data_webhook() {
        let provider = SourcehutProvider;
        let payload = br#"{"event": "something"}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("data.webhook"));
    }

    #[test]
    fn test_parse_push_event_missing_updates() {
        let provider = SourcehutProvider;
        let payload = br#"{"data":{"webhook":{"uuid":"test","event":"GIT_POST_RECEIVE"}}}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("updates"));
    }

    #[test]
    fn test_parse_push_event_empty_updates() {
        let provider = SourcehutProvider;
        let payload = br#"{"data":{"webhook":{"uuid":"test","updates":[]}}}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_parse_push_event_missing_ref_name() {
        let provider = SourcehutProvider;
        let payload =
            br#"{"data":{"webhook":{"updates":[{"old":{"id":"abc"},"new":{"id":"def"}}]}}}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ref.name"));
    }

    #[test]
    fn test_parse_push_event_missing_new_id() {
        let provider = SourcehutProvider;
        let payload = br#"{"data":{"webhook":{"updates":[{"ref":{"name":"refs/heads/main"},"old":{"id":"abc"}}]}}}"#;

        let result = provider.parse_push_event(payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("new.id"));
    }

    #[test]
    fn test_parse_push_event_multiple_updates_uses_first() {
        let provider = SourcehutProvider;
        let payload = br#"
            {
                "data": {
                    "webhook": {
                        "uuid": "test-uuid",
                        "event": "GIT_POST_RECEIVE",
                        "updates": [
                            {
                                "ref": { "name": "refs/heads/main" },
                                "old": { "id": "aaa111" },
                                "new": { "id": "bbb222" }
                            },
                            {
                                "ref": { "name": "refs/heads/pages" },
                                "old": { "id": "ccc333" },
                                "new": { "id": "ddd444" }
                            }
                        ]
                    }
                }
            }
        "#;

        let event = provider.parse_push_event(payload).unwrap();
        assert_eq!(event.ref_name, "refs/heads/main");
        assert_eq!(event.commit, "bbb222");
        assert!(!event.is_test);
    }
}

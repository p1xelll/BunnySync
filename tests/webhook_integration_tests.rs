//! Integration tests for webhook HTTP endpoints
//!
//! These tests verify endpoint behavior using axum's test utilities
//! without requiring actual network connections.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

// Import from the bunnysync crate
use bunnysync::config::{Config, ProjectConfig};
use bunnysync::webhook::create_router;

/// Creates a test configuration with a sample project
fn create_test_config() -> Arc<Config> {
    let mut projects = HashMap::new();
    projects.insert(
        "test-project".to_string(),
        ProjectConfig {
            repo_url: "https://git.com/test/repo.git".to_string(),
            webhook_secret: "test-secret-that-is-at-least-32-characters".to_string(),
            bunny_storage_zone: "test-zone".to_string(),
            bunny_storage_password: "test-password".to_string(),
            bunny_pull_zone_id: "12345".to_string(),
            bunny_pull_zone_domain: "test.example.com".to_string(),
            bunny_api_key: Some("test-api-key".to_string()),
            deploy_branch: None,
        },
    );

    Arc::new(Config {
        bind_addr: "127.0.0.1:3000".to_string(),
        bunny_api_key: "test-api-key".to_string(),
        projects,
    })
}

mod health_endpoint {
    use super::*;

    #[tokio::test]
    async fn returns_200_ok_and_healthy_body() {
        // Arrange
        let config = create_test_config();
        let app = create_router(config);

        let request = Request::builder()
            .uri("/health")
            .method("GET")
            .body(Body::empty())
            .expect("valid request");

        // Act
        let response = app.oneshot(request).await.expect("response received");

        // Assert
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body read");
        let body = String::from_utf8(body_bytes.to_vec()).expect("valid utf-8");
        assert_eq!(body, "healthy");
    }

    #[tokio::test]
    async fn handles_get_method_only() {
        let config = create_test_config();
        let app = create_router(config);

        // Test POST to /health - should return 405 Method Not Allowed
        let post_request = Request::builder()
            .uri("/health")
            .method("POST")
            .body(Body::empty())
            .expect("valid request");

        let response = app.clone().oneshot(post_request).await.expect("response");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

mod webhook_endpoint {
    use super::*;
    use axum::http::header;

    #[tokio::test]
    async fn returns_404_for_unknown_project() {
        // Arrange
        let config = create_test_config();
        let app = create_router(config);

        let request = Request::builder()
            .uri("/hook/unknown-project")
            .method("POST")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .expect("valid request");

        // Act
        let response = app.oneshot(request).await.expect("response received");

        // Assert
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body read");
        let body = String::from_utf8(body_bytes.to_vec()).expect("valid utf-8");
        assert_eq!(body, "project not found");
    }

    #[tokio::test]
    async fn returns_400_for_unknown_provider() {
        // Arrange - no provider-specific headers
        let config = create_test_config();
        let app = create_router(config);

        let request = Request::builder()
            .uri("/hook/test-project")
            .method("POST")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"ref":"refs/heads/main"}"#))
            .expect("valid request");

        // Act
        let response = app.oneshot(request).await.expect("response received");

        // Assert
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body read");
        let body = String::from_utf8(body_bytes.to_vec()).expect("valid utf-8");
        assert_eq!(body, "unknown provider");
    }

    #[tokio::test]
    async fn returns_401_for_invalid_signature() {
        // Arrange - Forgejo headers but invalid signature
        let config = create_test_config();
        let app = create_router(config);

        let request = Request::builder()
            .uri("/hook/test-project")
            .method("POST")
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-Forgejo-Event", "push")
            .header("X-Forgejo-Signature", "invalid-signature")
            .body(Body::from(r#"{"ref":"refs/heads/main"}"#))
            .expect("valid request");

        // Act
        let response = app.oneshot(request).await.expect("response received");

        // Assert
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body read");
        let body = String::from_utf8(body_bytes.to_vec()).expect("valid utf-8");
        assert_eq!(body, "invalid signature");
    }

    #[tokio::test]
    async fn returns_405_for_get_request() {
        // Arrange
        let config = create_test_config();
        let app = create_router(config);

        let request = Request::builder()
            .uri("/hook/test-project")
            .method("GET")
            .body(Body::empty())
            .expect("valid request");

        // Act
        let response = app.oneshot(request).await.expect("response received");

        // Assert
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

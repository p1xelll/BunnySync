//! BunnySync - Automated deployment service for BunnyCDN
//!
//! This crate provides webhook handlers for Git push events
//! and automated deployment to BunnyCDN storage zones.

pub mod bunny;
pub mod config;
pub mod deploy_queue;
pub mod diff;
pub mod providers;
pub mod signature_cache;
pub mod types;
pub mod webhook;

// Re-export commonly used types for convenience
pub use config::{Config, ProjectConfig};
pub use webhook::create_router;

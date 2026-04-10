use bunnysync::config::Config;
use bunnysync::webhook::create_router;
use std::sync::Arc;
use tracing::error;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = match Config::from_env() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("[startup] FATAL: invalid configuration — {}", e);
            std::process::exit(1);
        }
    };

    let app = create_router(config.clone());

    let listener = match tokio::net::TcpListener::bind(&config.bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("failed to bind to {}: {}", config.bind_addr, e);
            std::process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, app).await {
        error!("server error: {}", e);
        std::process::exit(1);
    }
}

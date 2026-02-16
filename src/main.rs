use axum::routing::get;
use axum::{Json, Router};
use axum_prometheus::PrometheusMetricLayer;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use realtime_notification_hub::config::AppConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    // Set RUST_LOG default before initializing tracing
    if std::env::var("RUST_LOG").is_err() {
        // SAFETY: Called before any threads are spawned (start of main).
        unsafe {
            std::env::set_var("RUST_LOG", "info,realtime_notification_hub=debug");
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();

    let config = AppConfig::from_env()?;
    let port = config.server_port;

    // Prometheus metrics
    let (prometheus_layer, metric_handle) = PrometheusMetricLayer::pair();

    // CORS
    let cors = build_cors_layer(&config);

    // Public routes (no auth)
    let public_routes = Router::new()
        .route("/health", get(health_handler))
        .route(
            "/metrics",
            get(move || {
                let handle = metric_handle.clone();
                async move { handle.render() }
            }),
        );

    // API v1 routes (placeholder for future phases)
    let api_v1_routes = Router::new();

    let app = Router::new()
        .merge(public_routes)
        .nest("/api/v1", api_v1_routes)
        .layer(TraceLayer::new_for_http())
        .layer(prometheus_layer)
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    tracing::info!("Notification Hub 시작: http://0.0.0.0:{}", port);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn build_cors_layer(config: &AppConfig) -> CorsLayer {
    let origins: Vec<_> = config
        .allowed_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(AllowMethods::list([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PATCH,
            axum::http::Method::OPTIONS,
        ]))
        .allow_headers(AllowHeaders::list([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            "Last-Event-ID".parse().unwrap(),
            "X-Internal-Key".parse().unwrap(),
        ]))
        .allow_credentials(true)
}

async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.ok();
    tracing::info!("Shutdown 신호 수신");
}

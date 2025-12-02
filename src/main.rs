// Copyright 2025 AtomArtist. All rights reserved.

use axum::{Router, routing::get};
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;

mod config;
mod routes;
mod db;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    dotenv::dotenv().ok();

    let config = config::Config::from_env();
    let db_pool = db::create_pool(&config.database_url)
        .await
        .expect("Failed to create database pool");

    // Run migrations
    sqlx::migrate!("./migrations")
        .run(&db_pool)
        .await
        .expect("Failed to run migrations");

    let app = create_app(db_pool);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    tracing::info!("AtomArtist listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap();

    axum::serve(listener, app)
        .await
        .unwrap();
}

fn create_app(db_pool: sqlx::PgPool) -> Router {
    Router::new()
        .route("/", get(routes::index))
        .route("/health", get(routes::health::health_check))
        .nest("/api", routes::api::router())
        .layer(TraceLayer::new_for_http())
        .with_state(db_pool)
}


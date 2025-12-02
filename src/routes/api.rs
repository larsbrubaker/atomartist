// Copyright 2025 AtomArtist. All rights reserved.

use axum::{Router, routing::get, Json};
use serde_json::{json, Value};

pub fn router() -> Router<sqlx::PgPool> {
    Router::new()
        .route("/", get(api_index))
}

async fn api_index() -> Json<Value> {
    Json(json!({
        "message": "AtomArtist API",
        "version": "v1"
    }))
}


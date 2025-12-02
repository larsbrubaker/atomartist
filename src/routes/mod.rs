// Copyright 2025 AtomArtist. All rights reserved.

pub mod health;
pub mod api;

use axum::Json;
use serde_json::{json, Value};

pub async fn index() -> Json<Value> {
    Json(json!({
        "message": "Welcome to AtomArtist!",
        "version": "0.1.0"
    }))
}


// Copyright 2025 AtomArtist. All rights reserved.

use axum::Json;
use serde_json::{json, Value};

pub async fn health_check() -> Json<Value> {
    Json(json!({
        "status": "healthy",
        "service": "atomartist"
    }))
}


//! `GET /api/health` (SPEC §10.3).

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::AppState;

#[derive(Debug, Serialize)]
pub(crate) struct Health {
    status: &'static str,
    version: &'static str,
}

pub(crate) async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: state.version(),
    })
}

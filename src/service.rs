use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::State,
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::model::{ObservationReport, VerdictClass};

pub struct CachedReport {
    pub report: ObservationReport,
    pub sampled: Instant,
}

#[derive(Clone)]
pub struct ApiState {
    pub snapshot: Arc<RwLock<CachedReport>>,
    pub stale_after: Duration,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/v1/observation", get(observation))
        .with_state(state)
        .layer(middleware::from_fn(headers))
}

async fn headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

async fn health_payload(state: &ApiState) -> (bool, Value) {
    let snapshot = state.snapshot.read().await;
    let report = &snapshot.report;
    let age = snapshot.sampled.elapsed();
    let stale = age >= state.stale_after;
    let complete = report.verdict.observed_nodes >= 2
        && report.verdict.comparable_nodes == report.verdict.observed_nodes
        && report.heights.reachable_nodes == report.verdict.observed_nodes;
    let ready = !stale
        && complete
        && matches!(
            report.verdict.class,
            VerdictClass::Agreement | VerdictClass::ProvenLag
        );
    (
        ready,
        json!({
            "version": env!("CARGO_PKG_VERSION"),
        "status": if stale { "stale" } else if ready { "ok" } else { "degraded" },
            "ready": ready,
            "stale": stale,
            "snapshotAgeMs": age.as_millis().min(u128::from(u64::MAX)) as u64,
            "staleAfterMs": state.stale_after.as_millis().min(u128::from(u64::MAX)) as u64,
            "observedAtMs": report.observed_at_ms,
            "completedAtMs": report.completed_at_ms,
            "durationMs": report.duration_ms,
            "observedNodes": report.verdict.observed_nodes,
            "comparableNodes": report.verdict.comparable_nodes,
            "reachableNodes": report.heights.reachable_nodes,
            "verdict": report.verdict.class,
            "ancestryChecks": report.ancestry_checks.len(),
        }),
    )
}

/// Liveness always returns 200; the payload separately describes evidence quality.
async fn health(State(state): State<ApiState>) -> Json<Value> {
    Json(health_payload(&state).await.1)
}

/// Readiness requires fresh, complete, multi-node evidence without observed conflicts.
async fn ready(State(state): State<ApiState>) -> (StatusCode, Json<Value>) {
    let (ready, payload) = health_payload(&state).await;
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(payload),
    )
}

async fn observation(State(state): State<ApiState>) -> Json<ObservationReport> {
    Json(state.snapshot.read().await.report.clone())
}

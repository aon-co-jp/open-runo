//! Poem-free handler implementations, migrated one at a time from
//! `handlers/*.rs` (which stay on `poem` until every handler here has an
//! equivalent and `lib.rs::build_app` switches over). Each function here
//! returns a `hyper_compat::Handler` closing over whatever state it needs,
//! matching the JSON shape/status codes of its poem counterpart exactly.

use crate::auth_hyper::check_api_key;
use crate::hyper_compat::{empty_status, json_response, Handler};
use crate::keyring::KeyGuardian;
use crate::state::AppState;
use hyper::StatusCode;
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
struct FederationStatusResponse {
    contributing_services: Vec<String>,
    type_count: usize,
    field_count: usize,
}

/// GET /api/federation/status — poem-free port of
/// `handlers::federation::federation_status`, now with the same
/// `X-Api-Key` gate as the poem route (see `auth_hyper::check_api_key`).
pub fn federation_status_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let schema = state
                .federation_schema
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let field_count: usize = schema.types.values().map(|f| f.len()).sum();
            json_response(
                StatusCode::OK,
                &FederationStatusResponse {
                    contributing_services: schema.contributing_services,
                    type_count: schema.types.len(),
                    field_count,
                },
            )
        })
    })
}

#[derive(Serialize)]
struct DbStatus {
    backend: &'static str,
    status: &'static str,
}

/// GET /api/db/status — poem-free port of `handlers::db::db_status`, gated
/// by the same `X-Api-Key` check as the poem route.
pub fn db_status_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            json_response(
                StatusCode::OK,
                &DbStatus {
                    backend: state.db.backend_name(),
                    status: "ok",
                },
            )
        })
    })
}

#[derive(Serialize)]
struct RoutingEntry {
    table: String,
    target: String,
}

#[derive(Serialize)]
struct RoutingInfo {
    default_target: String,
    entries: Vec<RoutingEntry>,
}

/// GET /api/db/routing — poem-free port of `handlers::db::db_routing`.
/// The routing table is a static description (see the poem handler's
/// doc comment), so this has no dependency on `state` beyond auth.
pub fn db_routing_handler(guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let entries = vec![
                RoutingEntry { table: "sessions".into(), target: "postgresql".into() },
                RoutingEntry { table: "api_keys".into(), target: "postgresql".into() },
                RoutingEntry { table: "rate_limits".into(), target: "postgresql".into() },
                RoutingEntry { table: "schemas".into(), target: "both".into() },
                RoutingEntry { table: "backup_jobs".into(), target: "both".into() },
                RoutingEntry { table: "persisted_queries".into(), target: "both".into() },
                RoutingEntry { table: "schema_history".into(), target: "aruaru-db".into() },
                RoutingEntry { table: "change_records".into(), target: "aruaru-db".into() },
                RoutingEntry { table: "audit_log".into(), target: "aruaru-db".into() },
            ];
            json_response(
                StatusCode::OK,
                &RoutingInfo {
                    default_target: "postgresql".into(),
                    entries,
                },
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyper_compat::{serve, Router};
    use crate::keyring::GuardianConfig;
    use hyper::Method;

    fn guardian(state: &Arc<AppState>) -> Arc<KeyGuardian> {
        Arc::new(KeyGuardian::new(Arc::clone(&state.db), GuardianConfig::from_env()))
    }

    #[tokio::test]
    async fn federation_status_reflects_composed_schema() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/federation/status",
            federation_status_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/federation/status"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["contributing_services"], serde_json::json!([]));
        assert_eq!(body["type_count"], 0);
        assert_eq!(body["field_count"], 0);
    }

    #[tokio::test]
    async fn federation_status_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/federation/status",
            federation_status_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/federation/status"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn db_status_reports_in_memory_backend() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/db/status",
            db_status_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["backend"], "in-memory");
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn db_status_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/db/status",
            db_status_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/status"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn db_routing_has_expected_tables() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::GET, "/api/db/routing", db_routing_handler(guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/routing"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert!(body["entries"].as_array().unwrap().len() >= 8);
    }

    #[tokio::test]
    async fn db_routing_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::GET, "/api/db/routing", db_routing_handler(guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/routing"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}

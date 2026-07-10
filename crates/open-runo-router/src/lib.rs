//! `open-runo-router`: the Gateway Router, open-runo's fast entrypoint.
//!
//! ## Endpoints
//!
//! | Method | Path                              | Description                        |
//! |--------|-----------------------------------|------------------------------------|
//! | GET    | `/health`                         | Service health check               |
//! | GET    | `/healthz`                        | Kubernetes-style health alias      |
//! | POST   | `/api/schemas`                    | Register a schema version          |
//! | GET    | `/api/schemas/:service`           | Latest schema for a service        |
//! | GET    | `/api/schemas/:service/history`   | Full schema history                |
//! | POST   | `/api/federation/compose`         | Compose service schemas            |
//! | GET    | `/api/federation/status`          | Current composed schema summary    |
//! | POST   | `/api/ai/route`                   | Select best AI provider            |
//! | GET    | `/api/db/status`                  | DB backend name & health           |
//! | GET    | `/api/db/routing`                 | Per-table routing decisions        |
//! | GET    | `/api/db/:table`                  | List all records in a table        |
//! | GET    | `/api/db/:table/:key`             | Get one record                     |
//! | PUT    | `/api/db/:table/:key`             | Upsert a record                    |
//! | DELETE | `/api/db/:table/:key`             | Delete a record                    |
//!
//! All `/api/*` routes require an `X-Api-Key` header (enforced by
//! [`auth::ApiKeyAuth`] middleware). Health routes are exempt.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audit;
pub mod auth;
pub mod auth_hyper;
pub mod handlers;
pub mod handlers_hyper;
pub mod hyper_compat;
pub mod keyring;
pub mod maintenance;
pub mod middleware;
pub mod middleware_hyper;
pub mod rate_limit;
pub mod state;
pub mod validation;

use auth::ApiKeyAuth;
use handlers::{
    ai_routing::route_request,
    cache::{ai_stats, purge_all_pages, purge_page},
    maintenance::{
        backup_export, backup_import, backup_restore_latest, integrity_check,
        migrate_export_csv, migrate_export_sql,
    },
    db::{db_delete, db_get, db_list, db_put, db_routing, db_status},
    events::stream_events,
    federation::{compose_schemas, federation_status},
    persisted_queries::{get_persisted_query, register_persisted_query},
    scim::{
        create_group, create_user, delete_group, delete_user, get_group, get_user, list_groups,
        list_users, replace_group, replace_user,
    },
    schemas::{get_schema, get_schema_history, register_schema},
};
use open_runo_core::Config;
use poem::{
    get, handler, post,
    web::Json,
    Endpoint, EndpointExt, Route,
};
use keyring::{GuardianConfig, KeyGuardian};
use middleware::html_cache::{HtmlCacheConfig, HtmlCacheMiddleware, HtmlPageCache};
use rate_limit::RateLimit;
use serde::Serialize;
use state::AppState;
use std::sync::Arc;

// ── Health ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Health {
    status: &'static str,
    service: &'static str,
    version: &'static str,
}

#[handler]
fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        service: "open-runo-router",
        version: env!("CARGO_PKG_VERSION"),
    })
}

// ── App builder ────────────────────────────────────────────────────────────

/// Build the root [`Route`] for the gateway.
///
/// `state` is the shared in-memory store for schemas, federation, history, and db.
/// `rate_limit` enforces per-client request budgets.
pub fn build_app(state: Arc<AppState>, rate_limit: RateLimit) -> impl Endpoint {
    build_app_with_auth(state, rate_limit, ApiKeyAuth::from_env())
}

/// Like [`build_app`], but takes an explicit [`ApiKeyAuth`] so callers (and
/// tests) can control whether JWT bearer-token auth is enabled without
/// touching environment variables.
pub fn build_app_with_auth(
    state: Arc<AppState>,
    rate_limit: RateLimit,
    auth: ApiKeyAuth,
) -> impl Endpoint {
    // HTML page cache (env-configured; disabled unless OPEN_RUNO_HTML_CACHE=on).
    // Shared with the purge handlers via `Data`, and applied outermost so
    // cached pages skip the whole stack. API paths always bypass it.
    let page_cache = Arc::new(HtmlPageCache::new(HtmlCacheConfig::from_env()));

    // Self-operating API-key registry: auto-issued via SCIM, auto-verified
    // in auth, anomaly-quarantined by learned usage. While empty (dev), any
    // non-empty key is accepted, exactly as before.
    let guardian = Arc::new(KeyGuardian::new(Arc::clone(&state.db), GuardianConfig::from_env()));
    let auth = auth.with_guardian(Arc::clone(&guardian));

    // Self-maintenance: restore the learned AI model, then keep saving it,
    // reconciling the two databases, and (optionally) writing portable
    // backups — all in the background, no human operation required.
    maintenance::spawn(Arc::clone(&state), Arc::clone(&page_cache));

    Route::new()
        // ── Public health probes (no auth) ──────────────────────────────
        .at("/health",  get(health))
        .at("/healthz", get(health))
        // ── Schema Registry ──────────────────────────────────────────────
        .at("/api/schemas",                 post(register_schema))
        .at("/api/schemas/:service",        get(get_schema))
        .at("/api/schemas/:service/history",get(get_schema_history))
        // ── Federation Engine ────────────────────────────────────────────
        .at("/api/federation/compose",  post(compose_schemas))
        .at("/api/federation/status",   get(federation_status))
        // ── AI Routing Engine ────────────────────────────────────────────
        .at("/api/ai/route", post(route_request))
        // ── DUAL DATABASE ────────────────────────────────────────────────
        .at("/api/db/status",          get(db_status))
        .at("/api/db/routing",         get(db_routing))
        .at("/api/db/:table",          get(db_list))
        .at("/api/db/:table/:key",     get(db_get).put(db_put).delete(db_delete))
        // ── HTML page cache administration ───────────────────────────────
        .at("/api/cache/purge",     post(purge_page))
        .at("/api/cache/purge-all", post(purge_all_pages))
        .at("/api/cache/ai-stats",  get(ai_stats))
        // ── Self-maintenance: backups + integrity ────────────────────────
        .at("/api/backup/export",   post(backup_export))
        .at("/api/backup/import",   post(backup_import))
        .at("/api/backup/restore-latest", post(backup_restore_latest))
        .at("/api/migrate/export-sql", post(migrate_export_sql))
        .at("/api/migrate/export-csv", post(migrate_export_csv))
        .at("/api/integrity/check", post(integrity_check))
        // ── Persisted Queries / Trusted Documents ────────────────────────
        .at("/api/persisted-queries",       post(register_persisted_query))
        .at("/api/persisted-queries/:hash", get(get_persisted_query))
        // ── SCIM 2.0 provisioning (RFC 7644) ─────────────────────────────
        .at("/scim/v2/Users",     get(list_users).post(create_user))
        .at("/scim/v2/Users/:id", get(get_user).put(replace_user).delete(delete_user))
        .at("/scim/v2/Groups",     get(list_groups).post(create_group))
        .at("/scim/v2/Groups/:id", get(get_group).put(replace_group).delete(delete_group))
        // ── Realtime events (SSE) ────────────────────────────────────────
        .at("/api/events", get(stream_events))
        // ── Middleware (applied outermost-first) ─────────────────────────
        .data(state)
        .data(guardian)
        .data(Arc::clone(&page_cache))
        .with(auth)
        .with(poem::middleware::Tracing)
        .with(rate_limit)
        .with(middleware::cors::build_cors())
        .with(HtmlCacheMiddleware(page_cache))
}

/// Resolve the bind address for the gateway from [`Config`].
pub fn bind_addr(config: &Config) -> &str {
    &config.bind_addr
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use poem::test::TestClient;
    use serde_json::json;

    fn app() -> impl Endpoint {
        build_app(Arc::new(AppState::new()), RateLimit::new(1_000, 60))
    }

    // ── health ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_returns_ok() {
        let client = TestClient::new(app());
        let resp = client.get("/health").send().await;
        resp.assert_status_is_ok();
        resp.assert_json(json!({
            "status": "ok",
            "service": "open-runo-router",
            "version": env!("CARGO_PKG_VERSION"),
        }))
        .await;
    }

    #[tokio::test]
    async fn healthz_alias_returns_ok() {
        let client = TestClient::new(app());
        client.get("/healthz").send().await.assert_status_is_ok();
    }

    // ── auth guard ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn api_route_requires_api_key() {
        let client = TestClient::new(app());
        client
            .get("/api/federation/status")
            .send()
            .await
            .assert_status(poem::http::StatusCode::UNAUTHORIZED);
    }

    // ── schemas ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn schema_register_and_fetch_roundtrip() {
        let client = TestClient::new(app());

        client
            .post("/api/schemas")
            .header("x-api-key", "test-key")
            .body_json(&json!({
                "service_name": "users",
                "sdl": "type User { id: ID! name: String }",
                "stage": "local"
            }))
            .send()
            .await
            .assert_status_is_ok();

        client
            .get("/api/schemas/users")
            .header("x-api-key", "test-key")
            .send()
            .await
            .assert_status_is_ok();
    }

    // ── federation ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn federation_compose_and_status() {
        let client = TestClient::new(app());

        client
            .post("/api/federation/compose")
            .header("x-api-key", "test-key")
            .body_json(&json!({
                "services": [
                    { "service_name": "users",   "types": { "User":    ["id", "name"]   } },
                    { "service_name": "billing", "types": { "Invoice": ["id", "amount"] } }
                ]
            }))
            .send()
            .await
            .assert_status_is_ok();

        client
            .get("/api/federation/status")
            .header("x-api-key", "test-key")
            .send()
            .await
            .assert_status_is_ok();
    }

    // ── ai routing ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn ai_route_returns_best_provider() {
        let client = TestClient::new(app());
        client
            .post("/api/ai/route")
            .header("x-api-key", "test-key")
            .body_json(&json!({
                "policy": "cost",
                "min_context_length": 4000,
                "candidates": [
                    {
                        "provider": "local_llm",
                        "estimated_cost_usd_per_1k_tokens": 0.0,
                        "estimated_latency_ms": 900,
                        "is_local": true,
                        "context_length": 8000
                    },
                    {
                        "provider": "anthropic",
                        "estimated_cost_usd_per_1k_tokens": 3.0,
                        "estimated_latency_ms": 400,
                        "is_local": false,
                        "context_length": 200000
                    }
                ]
            }))
            .send()
            .await
            .assert_status_is_ok();
    }

    // ── /api/db/* ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn db_status_returns_ok() {
        let client = TestClient::new(app());
        let resp = client
            .get("/api/db/status")
            .header("x-api-key", "test-key")
            .send()
            .await;
        resp.assert_status_is_ok();
        // InMemoryBackend in test mode
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["backend"], "in-memory");
        assert_eq!(body["status"],  "ok");
    }

    #[tokio::test]
    async fn db_routing_has_expected_tables() {
        let client = TestClient::new(app());
        let resp = client
            .get("/api/db/routing")
            .header("x-api-key", "test-key")
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert!(body["entries"].as_array().unwrap().len() >= 8);
    }

    #[tokio::test]
    async fn db_crud_roundtrip() {
        let client = TestClient::new(app());
        let key = "x-api-key";

        // PUT (upsert)
        client
            .put("/api/db/test_table/rec1")
            .header(key, "test-key")
            .body_json(&json!({ "value": { "hello": "world" } }))
            .send()
            .await
            .assert_status_is_ok();

        // GET single record
        let resp = client
            .get("/api/db/test_table/rec1")
            .header(key, "test-key")
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["value"]["hello"], "world");

        // LIST table
        let list = client
            .get("/api/db/test_table")
            .header(key, "test-key")
            .send()
            .await;
        list.assert_status_is_ok();
        let list_body: serde_json::Value = list.json().await.value().deserialize();
        assert_eq!(list_body["count"], 1);

        // DELETE
        client
            .delete("/api/db/test_table/rec1")
            .header(key, "test-key")
            .send()
            .await
            .assert_status_is_ok();

        // Confirm gone (404)
        client
            .get("/api/db/test_table/rec1")
            .header(key, "test-key")
            .send()
            .await
            .assert_status(poem::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn db_get_missing_key_returns_404() {
        let client = TestClient::new(app());
        client
            .get("/api/db/schemas/nonexistent")
            .header("x-api-key", "test-key")
            .send()
            .await
            .assert_status(poem::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mutations_leave_audit_trail() {
        let client = TestClient::new(app());

        // 1 mutation: register a schema.
        client
            .post("/api/schemas")
            .header("x-api-key", "test-key")
            .body_json(&json!({
                "service_name": "audited",
                "sdl": "type Q { x: ID }",
                "stage": "local"
            }))
            .send()
            .await
            .assert_status_is_ok();

        // 2nd mutation: db put.
        client
            .put("/api/db/some_table/k1")
            .header("x-api-key", "test-key")
            .body_json(&json!({ "value": { "v": 1 } }))
            .send()
            .await
            .assert_status_is_ok();

        // audit_log should now hold 2 records.
        let resp = client
            .get("/api/db/audit_log")
            .header("x-api-key", "test-key")
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["count"], 2);
        let actions: Vec<&str> = body["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["value"]["action"].as_str().unwrap())
            .collect();
        assert!(actions.contains(&"schema.register"));
        assert!(actions.contains(&"db.put"));
    }

    #[tokio::test]
    async fn persisted_query_register_and_fetch_roundtrip() {
        let client = TestClient::new(app());

        let resp = client
            .post("/api/persisted-queries")
            .header("x-api-key", "test-key")
            .body_json(&json!({ "query": "{ health }" }))
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        let hash = body["hash"].as_str().unwrap().to_string();
        assert_eq!(hash.len(), 64);

        let resp = client
            .get(format!("/api/persisted-queries/{hash}"))
            .header("x-api-key", "test-key")
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["query"], "{ health }");

        // Unknown hash → 404.
        client
            .get("/api/persisted-queries/0000000000000000000000000000000000000000000000000000000000000000")
            .header("x-api-key", "test-key")
            .send()
            .await
            .assert_status(poem::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn scim_user_lifecycle_roundtrip() {
        let client = TestClient::new(app());
        let key = "x-api-key";

        // Create → 201 with server-assigned id + SCIM meta.
        let resp = client
            .post("/scim/v2/Users")
            .header(key, "test-key")
            .body_json(&json!({
                "userName": "alice@example.com",
                "displayName": "Alice",
                "emails": [{ "value": "alice@example.com", "primary": true }],
                "roles": ["developer"]
            }))
            .send()
            .await;
        resp.assert_status(poem::http::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.value().deserialize();
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["userName"], "alice@example.com");
        assert_eq!(body["meta"]["resourceType"], "User");

        // KeyGuardian auto-issued a real key with the user; from here the
        // registry is non-empty, so we authenticate with the issued key.
        let issued = body["urn:open-runo:params:scim:api-key"]
            .as_str()
            .unwrap()
            .to_string();

        // Duplicate userName → 409.
        client
            .post("/scim/v2/Users")
            .header(key, &issued)
            .body_json(&json!({ "userName": "alice@example.com" }))
            .send()
            .await
            .assert_status(poem::http::StatusCode::CONFLICT);

        // List with RFC 7644 filter.
        let resp = client
            .get("/scim/v2/Users")
            .query("filter", &r#"userName eq "alice@example.com""#)
            .header(key, &issued)
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["totalResults"], 1);

        // Replace: deprovision (active=false) → the key is AUTO-REVOKED.
        let resp = client
            .put(format!("/scim/v2/Users/{id}"))
            .header(key, &issued)
            .body_json(&json!({ "userName": "alice@example.com", "active": false }))
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["active"], false);

        // The revoked key no longer authenticates (self-defending registry).
        client
            .get(format!("/scim/v2/Users/{id}"))
            .header(key, &issued)
            .send()
            .await
            .assert_status(poem::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn namespaces_isolate_schemas_over_rest() {
        let client = TestClient::new(app());
        let key = "x-api-key";

        client
            .post("/api/schemas")
            .header(key, "test-key")
            .body_json(&json!({
                "service_name": "users",
                "sdl": "type EgovUser { id: ID }",
                "namespace": "e-gov"
            }))
            .send()
            .await
            .assert_status_is_ok();

        // Same service name, different namespace.
        let resp = client
            .get("/api/schemas/users")
            .query("namespace", &"e-gov")
            .header(key, "test-key")
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["namespace"], "e-gov");

        // Default namespace has no such schema → 404.
        client
            .get("/api/schemas/users")
            .header(key, "test-key")
            .send()
            .await
            .assert_status(poem::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn scim_group_lifecycle_roundtrip() {
        let client = TestClient::new(app());
        let key = "x-api-key";

        let resp = client
            .post("/scim/v2/Groups")
            .header(key, "test-key")
            .body_json(&json!({
                "displayName": "engineering",
                "members": [{ "value": "user-1", "display": "Alice" }]
            }))
            .send()
            .await;
        resp.assert_status(poem::http::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.value().deserialize();
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["meta"]["resourceType"], "Group");

        // Replace membership (IdP sync).
        let resp = client
            .put(format!("/scim/v2/Groups/{id}"))
            .header(key, "test-key")
            .body_json(&json!({
                "displayName": "engineering",
                "members": [
                    { "value": "user-1" },
                    { "value": "user-2" }
                ]
            }))
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["members"].as_array().unwrap().len(), 2);

        client
            .delete(format!("/scim/v2/Groups/{id}"))
            .header(key, "test-key")
            .send()
            .await
            .assert_status(poem::http::StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn cache_purge_endpoints_respond() {
        let client = TestClient::new(app());
        let key = "x-api-key";

        let resp = client
            .post("/api/cache/purge")
            .header(key, "test-key")
            .body_json(&json!({ "path": "/page/123" }))
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["purged"], "/page/123");

        client
            .post("/api/cache/purge-all")
            .header(key, "test-key")
            .send()
            .await
            .assert_status_is_ok();
    }

    #[tokio::test]
    async fn key_guardian_full_auto_lifecycle() {
        let client = TestClient::new(app());

        // Dev mode: registry empty → any key passes (unchanged behaviour).
        client
            .get("/api/db/status")
            .header("x-api-key", "anything-goes")
            .send()
            .await
            .assert_status_is_ok();

        // Provision a user via SCIM → a key is AUTO-ISSUED in the response.
        let resp = client
            .post("/scim/v2/Users")
            .header("x-api-key", "bootstrap")
            .body_json(&json!({ "userName": "eve@example.com", "roles": ["developer"] }))
            .send()
            .await;
        resp.assert_status(poem::http::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.value().deserialize();
        let issued = body["urn:open-runo:params:scim:api-key"]
            .as_str()
            .unwrap()
            .to_string();
        let user_id = body["id"].as_str().unwrap().to_string();
        assert!(issued.starts_with("orn_"));

        // Registry is now non-empty → auto-hardening: random keys rejected…
        client
            .get("/api/db/status")
            .header("x-api-key", "anything-goes")
            .send()
            .await
            .assert_status(poem::http::StatusCode::UNAUTHORIZED);

        // …while the auto-issued key verifies.
        client
            .get("/api/db/status")
            .header("x-api-key", &issued)
            .send()
            .await
            .assert_status_is_ok();

        // Deleting the user AUTO-REVOKES their key.
        client
            .delete(format!("/scim/v2/Users/{user_id}"))
            .header("x-api-key", &issued)
            .send()
            .await
            .assert_status(poem::http::StatusCode::NO_CONTENT);
        client
            .get("/api/db/status")
            .header("x-api-key", &issued)
            .send()
            .await
            .assert_status(poem::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn backup_and_integrity_endpoints_respond() {
        let client = TestClient::new(app());
        let key = "x-api-key";

        // Register something so the export has content.
        client
            .post("/api/schemas")
            .header(key, "test-key")
            .body_json(&json!({ "service_name": "bk", "sdl": "type B { x: ID }" }))
            .send()
            .await
            .assert_status_is_ok();

        // Integrity check: InMemory backend → nothing to heal, still 200.
        let resp = client
            .post("/api/integrity/check")
            .header(key, "test-key")
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        assert_eq!(body["healed"], 0);

        // Export writes at least one portable file.
        let dir = std::env::temp_dir().join(format!("orn-e2e-{}", uuid::Uuid::new_v4()));
        std::env::set_var("OPEN_RUNO_BACKUP_DIR", &dir);
        let resp = client
            .post("/api/backup/export")
            .header(key, "test-key")
            .send()
            .await;
        resp.assert_status_is_ok();
        let body: serde_json::Value = resp.json().await.value().deserialize();
        let path = body["written"][0].as_str().unwrap().to_string();
        assert!(body["records"].as_u64().unwrap() >= 1);

        // Import the file back through the API.
        let resp = client
            .post("/api/backup/import")
            .header(key, "test-key")
            .body_json(&json!({ "path": path }))
            .send()
            .await;
        resp.assert_status_is_ok();

        std::env::remove_var("OPEN_RUNO_BACKUP_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}


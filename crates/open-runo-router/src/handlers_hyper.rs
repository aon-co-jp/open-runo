//! Poem-free handler implementations, migrated one at a time from
//! `handlers/*.rs` (which stay on `poem` until every handler here has an
//! equivalent and `lib.rs::build_app` switches over). Each function here
//! returns a `hyper_compat::Handler` closing over whatever state it needs,
//! matching the JSON shape/status codes of its poem counterpart exactly.

use crate::auth_hyper::check_api_key;
use crate::hyper_compat::{empty_status, json_response, query_params, read_json_body, Handler};
use crate::keyring::KeyGuardian;
use crate::state::AppState;
use crate::validation::{DB_UPSERT_REQUEST, REGISTER_SCHEMA_REQUEST};
use hyper::StatusCode;
use open_runo_schema_registry::{Stage, DEFAULT_NAMESPACE};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Poem-free equivalent of `audit::actor_from`: identify the caller from
/// the `X-Api-Key` header alone (JWT/Claims extraction isn't wired at this
/// layer yet, see auth_hyper.rs doc comment).
fn actor_from_headers(headers: &hyper::HeaderMap) -> String {
    match headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        Some(key) if !key.is_empty() => {
            let prefix: String = key.chars().take(4).collect();
            format!("api-key:{prefix}***")
        }
        _ => "anonymous".to_string(),
    }
}

fn parse_stage(s: &str) -> Stage {
    match s.to_lowercase().as_str() {
        "development" | "dev" => Stage::Development,
        "staging" | "stg" => Stage::Staging,
        "production" | "prod" => Stage::Production,
        _ => Stage::Local,
    }
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Local => "local",
        Stage::Development => "development",
        Stage::Staging => "staging",
        Stage::Production => "production",
    }
}

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

#[derive(Debug, Deserialize)]
struct ServiceInput {
    service_name: String,
    types: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ComposeRequest {
    services: Vec<ServiceInput>,
}

#[derive(Serialize)]
struct ComposeResponse {
    contributing_services: Vec<String>,
    types: std::collections::BTreeMap<String, Vec<String>>,
    breaking_changes: Vec<String>,
}

/// POST /api/federation/compose — poem-free port of
/// `handlers::federation::compose_schemas`.
pub fn compose_schemas_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let body: ComposeRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let service_schemas: Vec<open_runo_federation::ServiceSchema> = body
                .services
                .into_iter()
                .map(|s| open_runo_federation::ServiceSchema {
                    service_name: s.service_name,
                    types: s
                        .types
                        .into_iter()
                        .map(|(k, v)| (k, std::collections::BTreeSet::from_iter(v)))
                        .collect(),
                })
                .collect();

            let new_composed = match open_runo_federation::compose(&service_schemas) {
                Ok(c) => c,
                Err(e) => {
                    return json_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            let breaking = {
                let previous = state
                    .federation_schema
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if previous.contributing_services.is_empty() {
                    vec![]
                } else {
                    open_runo_federation::detect_breaking_changes(&previous, &new_composed)
                }
            };

            let contributing_services = new_composed.contributing_services.clone();
            let types_out: std::collections::BTreeMap<String, Vec<String>> = new_composed
                .types
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect();

            *state
                .federation_schema
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = new_composed;

            json_response(
                StatusCode::OK,
                &ComposeResponse {
                    contributing_services,
                    types: types_out,
                    breaking_changes: breaking,
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

fn parse_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or(serde_json::Value::String(raw.to_string()))
}

#[derive(Serialize)]
struct RecordItem {
    key: String,
    value: serde_json::Value,
}

#[derive(Serialize)]
struct RecordListResponse {
    table: String,
    count: usize,
    records: Vec<RecordItem>,
}

/// GET /api/db/:table — poem-free port of `handlers::db::db_list`.
pub fn db_list_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let table = params.get("table").unwrap_or("").to_string();
            let records = match state.db.list(&table).await {
                Ok(r) => r,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };
            let items: Vec<RecordItem> = records
                .into_iter()
                .map(|r| RecordItem { key: r.key, value: parse_value(&r.value) })
                .collect();
            json_response(
                StatusCode::OK,
                &RecordListResponse { count: items.len(), table, records: items },
            )
        })
    })
}

#[derive(Serialize)]
struct RecordResponse {
    table: String,
    key: String,
    value: serde_json::Value,
}

/// GET /api/db/:table/:key — poem-free port of `handlers::db::db_get`.
pub fn db_get_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let table = params.get("table").unwrap_or("").to_string();
            let key = params.get("key").unwrap_or("").to_string();
            match state.db.get(&table, &key).await {
                Ok(Some(raw)) => json_response(
                    StatusCode::OK,
                    &RecordResponse { table, key, value: parse_value(&raw) },
                ),
                Ok(None) => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("record not found: {table}/{key}") }),
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

#[derive(Deserialize)]
struct UpsertBody {
    value: serde_json::Value,
}

/// PUT /api/db/:table/:key — poem-free port of `handlers::db::db_put`.
pub fn db_put_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let table = params.get("table").unwrap_or("").to_string();
            let key = params.get("key").unwrap_or("").to_string();

            let raw: serde_json::Value = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let errors: Vec<String> = DB_UPSERT_REQUEST
                .iter_errors(&raw)
                .map(|e| format!("{} (at {})", e, e.instance_path))
                .collect();
            if !errors.is_empty() {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({
                        "error": format!("request body failed validation: {}", errors.join("; "))
                    }),
                );
            }

            let body: UpsertBody = match serde_json::from_value(raw) {
                Ok(b) => b,
                Err(e) => {
                    return json_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &serde_json::json!({ "error": format!("deserialize body: {e}") }),
                    )
                }
            };

            let serialized = match serde_json::to_string(&body.value) {
                Ok(s) => s,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": format!("serialize value: {e}") }),
                    )
                }
            };

            if let Err(e) = state.db.put(&table, &key, &serialized).await {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                );
            }

            crate::audit::record(&state, &actor, "db.put", format!("{table}/{key}")).await;

            json_response(
                StatusCode::OK,
                &RecordResponse { table, key, value: body.value },
            )
        })
    })
}

#[derive(Serialize)]
struct DeleteResponse {
    table: String,
    key: String,
    deleted: bool,
}

/// DELETE /api/db/:table/:key — poem-free port of `handlers::db::db_delete`.
pub fn db_delete_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let table = params.get("table").unwrap_or("").to_string();
            let key = params.get("key").unwrap_or("").to_string();

            if let Err(e) = state.db.delete(&table, &key).await {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                );
            }

            crate::audit::record(&state, &actor, "db.delete", format!("{table}/{key}")).await;

            json_response(
                StatusCode::OK,
                &DeleteResponse { table, key, deleted: true },
            )
        })
    })
}

#[derive(Serialize)]
struct SchemaResponse {
    id: String,
    namespace: String,
    service_name: String,
    sdl: String,
    stage: String,
    created_at: String,
}

/// GET /api/schemas/:service — poem-free port of `handlers::schemas::get_schema`.
pub fn get_schema_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let query = query_params(&req);
            let service = params.get("service").unwrap_or("").to_string();
            let stage_str = query.get("stage").map(String::as_str).unwrap_or("local");
            let stage = parse_stage(stage_str);
            let namespace = query
                .get("namespace")
                .map(String::as_str)
                .unwrap_or(DEFAULT_NAMESPACE);

            let registry = state
                .schema_registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            match registry.latest_in(namespace, &service, stage) {
                Some(v) => json_response(
                    StatusCode::OK,
                    &SchemaResponse {
                        id: v.id.to_string(),
                        namespace: v.namespace.clone(),
                        service_name: v.service_name.clone(),
                        sdl: v.sdl.clone(),
                        stage: stage_name(v.stage).to_string(),
                        created_at: v.created_at.to_rfc3339(),
                    },
                ),
                None => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({
                        "error": format!("no schema found for '{service}' at stage '{stage_str}'")
                    }),
                ),
            }
        })
    })
}

#[derive(Serialize)]
struct HistoryResponse {
    versions: Vec<SchemaResponse>,
}

/// GET /api/schemas/:service/history — poem-free port of
/// `handlers::schemas::get_schema_history`.
pub fn get_schema_history_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let query = query_params(&req);
            let service = params.get("service").unwrap_or("").to_string();
            let namespace = query
                .get("namespace")
                .map(String::as_str)
                .unwrap_or(DEFAULT_NAMESPACE);

            let registry = state
                .schema_registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            let versions = registry
                .history_in(namespace, &service)
                .iter()
                .map(|v| SchemaResponse {
                    id: v.id.to_string(),
                    namespace: v.namespace.clone(),
                    service_name: v.service_name.clone(),
                    sdl: v.sdl.clone(),
                    stage: stage_name(v.stage).to_string(),
                    created_at: v.created_at.to_rfc3339(),
                })
                .collect();

            json_response(StatusCode::OK, &HistoryResponse { versions })
        })
    })
}

fn default_stage() -> String {
    "local".to_string()
}

#[derive(Debug, Deserialize)]
struct RegisterRequest {
    service_name: String,
    sdl: String,
    #[serde(default = "default_stage")]
    stage: String,
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Serialize)]
struct RegisterResponse {
    id: String,
    namespace: String,
    service_name: String,
    stage: String,
    created_at: String,
}

/// POST /api/schemas — poem-free port of `handlers::schemas::register_schema`.
pub fn register_schema_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());

            let raw: serde_json::Value = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let errors: Vec<String> = REGISTER_SCHEMA_REQUEST
                .iter_errors(&raw)
                .map(|e| format!("{} (at {})", e, e.instance_path))
                .collect();
            if !errors.is_empty() {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({
                        "error": format!("request body failed validation: {}", errors.join("; "))
                    }),
                );
            }

            let body: RegisterRequest = match serde_json::from_value(raw) {
                Ok(b) => b,
                Err(e) => {
                    return json_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            let stage = parse_stage(&body.stage);
            let namespace = body
                .namespace
                .clone()
                .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
            let version = state
                .schema_registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .register_in(&namespace, &body.service_name, &body.sdl, stage);

            crate::audit::record(
                &state,
                &actor,
                "schema.register",
                format!("{}@{}", body.service_name, body.stage),
            )
            .await;

            let _ = state.events.send(crate::state::SchemaEvent {
                service_name: body.service_name.clone(),
                stage: body.stage.clone(),
                at: chrono::Utc::now().to_rfc3339(),
            });

            json_response(
                StatusCode::OK,
                &RegisterResponse {
                    id: version.id.to_string(),
                    namespace: version.namespace.clone(),
                    service_name: version.service_name.clone(),
                    stage: stage_name(version.stage).to_string(),
                    created_at: version.created_at.to_rfc3339(),
                },
            )
        })
    })
}

fn parse_provider(s: &str) -> open_runo_ai_routing::Provider {
    use open_runo_ai_routing::Provider;
    match s.to_lowercase().replace('-', "_").as_str() {
        "openai" => Provider::OpenAi,
        "anthropic" | "anthropic_claude" => Provider::AnthropicClaude,
        "google" | "google_gemini" | "gemini" => Provider::GoogleGemini,
        "deepseek" => Provider::DeepSeek,
        "local" | "local_llm" => Provider::LocalLlm,
        _ => Provider::CustomOpenAiCompatible,
    }
}

fn parse_policy(s: &str) -> open_runo_ai_routing::RoutingPolicy {
    use open_runo_ai_routing::RoutingPolicy;
    match s.to_lowercase().as_str() {
        "latency" | "latency_optimized" => RoutingPolicy::LatencyOptimized,
        "local" | "local_first" => RoutingPolicy::LocalFirst,
        "privacy" | "privacy_first" => RoutingPolicy::PrivacyFirst,
        _ => RoutingPolicy::CostOptimized,
    }
}

fn provider_name(p: &open_runo_ai_routing::Provider) -> &'static str {
    use open_runo_ai_routing::Provider;
    match p {
        Provider::OpenAi => "openai",
        Provider::AnthropicClaude => "anthropic_claude",
        Provider::GoogleGemini => "google_gemini",
        Provider::DeepSeek => "deepseek",
        Provider::LocalLlm => "local_llm",
        Provider::CustomOpenAiCompatible => "custom_openai_compatible",
    }
}

#[derive(Debug, Deserialize)]
struct CandidateInput {
    provider: String,
    estimated_cost_usd_per_1k_tokens: f64,
    estimated_latency_ms: u32,
    is_local: bool,
    context_length: u32,
}

#[derive(Debug, Deserialize)]
struct RouteRequest {
    policy: String,
    min_context_length: Option<u32>,
    candidates: Vec<CandidateInput>,
}

#[derive(Serialize)]
struct RouteResponse {
    selected_provider: String,
    is_local: bool,
    estimated_cost_usd_per_1k_tokens: f64,
    estimated_latency_ms: u32,
}

/// POST /api/ai/route — poem-free port of `handlers::ai_routing::route_request`.
pub fn route_request_handler(guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let body: RouteRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let candidates: Vec<open_runo_ai_routing::Candidate> = body
                .candidates
                .iter()
                .map(|c| open_runo_ai_routing::Candidate {
                    provider: parse_provider(&c.provider),
                    estimated_cost_usd_per_1k_tokens: c.estimated_cost_usd_per_1k_tokens,
                    estimated_latency_ms: c.estimated_latency_ms,
                    is_local: c.is_local,
                    context_length: c.context_length,
                })
                .collect();

            let policy = parse_policy(&body.policy);
            let min_ctx = body.min_context_length.unwrap_or(0);

            match open_runo_ai_routing::route(&candidates, policy, min_ctx) {
                Ok(chosen) => json_response(
                    StatusCode::OK,
                    &RouteResponse {
                        selected_provider: provider_name(&chosen.provider).to_string(),
                        is_local: chosen.is_local,
                        estimated_cost_usd_per_1k_tokens: chosen.estimated_cost_usd_per_1k_tokens,
                        estimated_latency_ms: chosen.estimated_latency_ms,
                    },
                ),
                Err(e) => json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
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

    #[tokio::test]
    async fn get_schema_returns_registered_version() {
        use open_runo_schema_registry::Stage;

        let state = Arc::new(AppState::new());
        state
            .schema_registry
            .lock()
            .unwrap()
            .register_in("default", "users", "type User { id: ID! }", Stage::Local);

        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/schemas/:service",
            get_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/schemas/users"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["service_name"], "users");
        assert_eq!(body["sdl"], "type User { id: ID! }");
    }

    #[tokio::test]
    async fn get_schema_missing_service_returns_404() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/schemas/:service",
            get_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/schemas/nonexistent"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_schema_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/schemas/:service",
            get_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/schemas/users"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_schema_history_returns_all_versions() {
        use open_runo_schema_registry::Stage;

        let state = Arc::new(AppState::new());
        {
            let mut registry = state.schema_registry.lock().unwrap();
            registry.register_in("default", "users", "type User { id: ID! }", Stage::Local);
            registry.register_in("default", "users", "type User { id: ID! name: String }", Stage::Local);
        }

        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::GET,
            "/api/schemas/:service/history",
            get_schema_history_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/schemas/users/history"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["versions"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn register_and_fetch_schema_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/schemas",
                register_schema_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/schemas/:service",
                get_schema_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/schemas"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({
                "service_name": "users",
                "sdl": "type User { id: ID! name: String }",
                "stage": "local"
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["service_name"], "users");

        let resp = client
            .get(format!("http://{addr}/api/schemas/users"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn register_schema_rejects_invalid_body() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/schemas",
            register_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/schemas"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "not_a_valid_field": true }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn register_schema_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/schemas",
            register_schema_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/schemas"))
            .json(&serde_json::json!({ "service_name": "users", "sdl": "type User { id: ID! }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ai_route_returns_best_provider() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::POST, "/api/ai/route", route_request_handler(guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/ai/route"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({
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
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["selected_provider"], "local_llm");
    }

    #[tokio::test]
    async fn ai_route_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::POST, "/api/ai/route", route_request_handler(guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/ai/route"))
            .json(&serde_json::json!({ "policy": "cost", "candidates": [] }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn compose_and_status_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/federation/compose",
                compose_schemas_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/federation/status",
                federation_status_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/federation/compose"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({
                "services": [
                    { "service_name": "users", "types": { "User": ["id", "name"] } },
                    { "service_name": "billing", "types": { "Invoice": ["id", "amount"] } }
                ]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/federation/status"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["type_count"], 2);
    }

    #[tokio::test]
    async fn compose_schemas_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/federation/compose",
            compose_schemas_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/federation/compose"))
            .json(&serde_json::json!({ "services": [] }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    fn db_router(state: &Arc<AppState>, guardian: Arc<KeyGuardian>) -> Router {
        Router::new()
            .route(Method::GET, "/api/db/:table", db_list_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::GET, "/api/db/:table/:key", db_get_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::PUT, "/api/db/:table/:key", db_put_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::DELETE, "/api/db/:table/:key", db_delete_handler(Arc::clone(state), guardian))
    }

    #[tokio::test]
    async fn db_crud_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = db_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();
        let key = "x-api-key";

        let resp = client
            .put(format!("http://{addr}/api/db/test_table/rec1"))
            .header(key, "test-key")
            .json(&serde_json::json!({ "value": { "hello": "world" } }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/db/test_table/rec1"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["value"]["hello"], "world");

        let resp = client
            .get(format!("http://{addr}/api/db/test_table"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["count"], 1);

        let resp = client
            .delete(format!("http://{addr}/api/db/test_table/rec1"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/api/db/test_table/rec1"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn db_get_missing_key_returns_404() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = db_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/db/schemas/nonexistent"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn db_put_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = db_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .put(format!("http://{addr}/api/db/test_table/rec1"))
            .json(&serde_json::json!({ "value": 1 }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}

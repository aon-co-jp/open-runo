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

fn persisted_query_store(state: &AppState) -> open_runo_persisted_queries::PersistedQueryStore {
    open_runo_persisted_queries::PersistedQueryStore::new(
        Arc::clone(&state.db),
        open_runo_persisted_queries::EnforcementMode::Allow,
    )
}

#[derive(Deserialize)]
struct PqRegisterRequest {
    query: String,
}

#[derive(Serialize)]
struct PqRegisterResponse {
    hash: String,
    registered_at: String,
}

/// POST /api/persisted-queries — poem-free port of
/// `handlers::persisted_queries::register_persisted_query`.
pub fn register_persisted_query_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let body: PqRegisterRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let record = match persisted_query_store(&state).register(&body.query).await {
                Ok(r) => r,
                Err(e) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            crate::audit::record(&state, &actor, "persisted_query.register", record.hash.clone()).await;

            json_response(
                StatusCode::OK,
                &PqRegisterResponse {
                    hash: record.hash,
                    registered_at: record.registered_at.to_rfc3339(),
                },
            )
        })
    })
}

#[derive(Serialize)]
struct PqQueryResponse {
    hash: String,
    query: String,
    registered_at: String,
}

/// GET /api/persisted-queries/:hash — poem-free port of
/// `handlers::persisted_queries::get_persisted_query`.
pub fn get_persisted_query_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let hash = params.get("hash").unwrap_or("").to_string();
            match persisted_query_store(&state).get(&hash).await {
                Ok(Some(record)) => json_response(
                    StatusCode::OK,
                    &PqQueryResponse {
                        hash: record.hash,
                        query: record.document,
                        registered_at: record.registered_at.to_rfc3339(),
                    },
                ),
                Ok(None) => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("persisted query not found: {hash}") }),
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
struct PurgeRequest {
    path: String,
}

#[derive(Serialize)]
struct PurgeResponse {
    purged: String,
}

/// POST /api/cache/purge — poem-free port of `handlers::cache::purge_page`.
pub fn purge_page_handler(
    state: Arc<AppState>,
    cache: Arc<crate::middleware::html_cache::HtmlPageCache>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let body: PurgeRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            cache.purge(&body.path).await;
            crate::audit::record(&state, &actor, "cache.purge", body.path.clone()).await;
            json_response(StatusCode::OK, &PurgeResponse { purged: body.path })
        })
    })
}

/// POST /api/cache/purge-all — poem-free port of
/// `handlers::cache::purge_all_pages`.
pub fn purge_all_pages_handler(
    state: Arc<AppState>,
    cache: Arc<crate::middleware::html_cache::HtmlPageCache>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            cache.purge_all().await;
            crate::audit::record(&state, &actor, "cache.purge_all", "*").await;
            json_response(StatusCode::OK, &PurgeResponse { purged: "*".to_string() })
        })
    })
}

#[derive(Serialize)]
struct PatternStat {
    pattern: String,
    requests: u64,
    arrival_interval_secs: Option<f64>,
    update_interval_secs: Option<f64>,
    render_cost_secs: Option<f64>,
}

#[derive(Serialize)]
struct AiStatsResponse {
    ai_enabled: bool,
    cache_hits: u64,
    cache_misses: u64,
    admitted: u64,
    rejected: u64,
    hit_ratio: f64,
    tracked_keys: usize,
    tracked_patterns: usize,
    top_patterns: Vec<PatternStat>,
}

/// GET /api/cache/ai-stats — poem-free port of `handlers::cache::ai_stats`.
pub fn ai_stats_handler(
    cache: Arc<crate::middleware::html_cache::HtmlPageCache>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let cache = Arc::clone(&cache);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let resp = match cache.predictor() {
                None => AiStatsResponse {
                    ai_enabled: false,
                    cache_hits: 0,
                    cache_misses: 0,
                    admitted: 0,
                    rejected: 0,
                    hit_ratio: 0.0,
                    tracked_keys: 0,
                    tracked_patterns: 0,
                    top_patterns: Vec::new(),
                },
                Some(p) => {
                    let snap = p.snapshot();
                    let total = snap.outcomes.cache_hits + snap.outcomes.cache_misses;
                    let mut patterns: Vec<PatternStat> = snap
                        .patterns
                        .iter()
                        .map(|(k, s)| PatternStat {
                            pattern: k.clone(),
                            requests: s.requests,
                            arrival_interval_secs: s.arrival_interval_secs,
                            update_interval_secs: s.update_interval_secs,
                            render_cost_secs: s.render_cost_secs,
                        })
                        .collect();
                    patterns.sort_by(|a, b| b.requests.cmp(&a.requests));
                    patterns.truncate(20);

                    AiStatsResponse {
                        ai_enabled: true,
                        cache_hits: snap.outcomes.cache_hits,
                        cache_misses: snap.outcomes.cache_misses,
                        admitted: snap.outcomes.admitted,
                        rejected: snap.outcomes.rejected,
                        hit_ratio: if total == 0 {
                            0.0
                        } else {
                            snap.outcomes.cache_hits as f64 / total as f64
                        },
                        tracked_keys: snap.keys.len(),
                        tracked_patterns: snap.patterns.len(),
                        top_patterns: patterns,
                    }
                }
            };
            json_response(StatusCode::OK, &resp)
        })
    })
}

#[derive(Serialize)]
struct ExportResponse {
    written: Vec<String>,
    records: usize,
}

/// POST /api/backup/export — poem-free port of `handlers::maintenance::backup_export`.
pub fn backup_export_handler(
    state: Arc<AppState>,
    cache: Arc<crate::middleware::html_cache::HtmlPageCache>,
    guardian: Arc<KeyGuardian>,
) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let config = crate::maintenance::BackupConfig::from_env();
            let (written, records) = match crate::maintenance::export_backup(&state, &cache, &config).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(
                &state,
                &actor,
                "backup.export",
                format!("{records} records → {}", written.join(", ")),
            )
            .await;
            json_response(StatusCode::OK, &ExportResponse { written, records })
        })
    })
}

#[derive(Deserialize)]
struct ImportRequest {
    path: String,
}

#[derive(Serialize)]
struct ImportResponse {
    restored: usize,
}

/// POST /api/backup/import — poem-free port of `handlers::maintenance::backup_import`.
pub fn backup_import_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let body: ImportRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let restored = match crate::maintenance::import_backup(&state, &body.path).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(
                &state,
                &actor,
                "backup.import",
                format!("{restored} records ← {}", body.path),
            )
            .await;
            json_response(StatusCode::OK, &ImportResponse { restored })
        })
    })
}

#[derive(Serialize)]
struct IntegrityResponse {
    backend: &'static str,
    healed: usize,
    discrepancies: Vec<open_runo_db::dual::Discrepancy>,
}

/// POST /api/integrity/check — poem-free port of `handlers::maintenance::integrity_check`.
pub fn integrity_check_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let discrepancies = match state.db.consistency_check_and_heal().await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };
            for d in &discrepancies {
                crate::audit::record(
                    &state,
                    &actor,
                    "integrity.heal",
                    format!("{}/{} {} (from {})", d.table, d.key, d.kind, d.healed_from),
                )
                .await;
            }
            json_response(
                StatusCode::OK,
                &IntegrityResponse {
                    backend: state.db.backend_name(),
                    healed: discrepancies.len(),
                    discrepancies,
                },
            )
        })
    })
}

#[derive(Serialize)]
struct RestoreLatestResponse {
    restored_from: String,
    restored: usize,
}

/// POST /api/backup/restore-latest — poem-free port of
/// `handlers::maintenance::backup_restore_latest`.
pub fn backup_restore_latest_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let config = crate::maintenance::BackupConfig::from_env();
            let path = match crate::maintenance::find_latest_backup(&config) {
                Some(p) => p,
                None => {
                    return json_response(
                        StatusCode::NOT_FOUND,
                        &serde_json::json!({ "error": "no backup file found" }),
                    )
                }
            };
            let path_str = path.display().to_string();
            let restored = match crate::maintenance::import_backup(&state, &path_str).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(
                &state,
                &actor,
                "backup.restore_latest",
                format!("{restored} records ← {path_str}"),
            )
            .await;
            json_response(
                StatusCode::OK,
                &RestoreLatestResponse { restored_from: path_str, restored },
            )
        })
    })
}

#[derive(Deserialize)]
struct ExportSqlRequest {
    dialect: crate::maintenance::SqlDialect,
}

#[derive(Serialize)]
struct ConversionResponse {
    written: Vec<String>,
}

/// POST /api/migrate/export-sql — poem-free port of
/// `handlers::maintenance::migrate_export_sql`.
pub fn migrate_export_sql_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let body: ExportSqlRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let sql = match crate::maintenance::export_sql(&state, body.dialect).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            let name = format!(
                "open-runo-dump-{:?}-{}.sql",
                body.dialect,
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            )
            .to_lowercase();
            let written = match crate::maintenance::write_to_backup_dirs(
                &crate::maintenance::BackupConfig::from_env(),
                &name,
                &sql,
            ) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(&state, &actor, "migrate.export_sql", written.join(", ")).await;
            json_response(StatusCode::OK, &ConversionResponse { written })
        })
    })
}

/// POST /api/migrate/export-csv — poem-free port of
/// `handlers::maintenance::migrate_export_csv`.
pub fn migrate_export_csv_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let csv = match crate::maintenance::export_csv(&state).await {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            let name = format!(
                "open-runo-dump-{}.csv",
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            );
            let written = match crate::maintenance::write_to_backup_dirs(
                &crate::maintenance::BackupConfig::from_env(),
                &name,
                &csv,
            ) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e }),
                    )
                }
            };
            crate::audit::record(&state, &actor, "migrate.export_csv", written.join(", ")).await;
            json_response(StatusCode::OK, &ConversionResponse { written })
        })
    })
}

fn scim_user_store(state: &AppState) -> open_runo_scim::ScimUserStore {
    open_runo_scim::ScimUserStore::new(Arc::clone(&state.db))
}

fn scim_group_store(state: &AppState) -> open_runo_scim::ScimGroupStore {
    open_runo_scim::ScimGroupStore::new(Arc::clone(&state.db))
}

/// GET /scim/v2/Users — poem-free port of `handlers::scim::list_users`.
pub fn scim_list_users_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let query = query_params(&req);
            let filter = query
                .get("filter")
                .and_then(|f| open_runo_scim::ScimUserStore::parse_user_name_filter(f));

            match scim_user_store(&state).list(filter.as_deref()).await {
                Ok(users) => json_response(
                    StatusCode::OK,
                    &open_runo_scim::ListResponse {
                        schemas: vec!["urn:ietf:params:scim:api:messages:2.0:ListResponse".into()],
                        total_results: users.len(),
                        resources: users,
                    },
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// POST /scim/v2/Users — poem-free port of `handlers::scim::create_user`,
/// including KeyGuardian's auto-issue on provisioning.
pub fn scim_create_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let input: open_runo_scim::UserInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let user = match scim_user_store(&state).create(input).await {
                Ok(u) => u,
                Err(e) => {
                    return json_response(
                        StatusCode::CONFLICT,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            crate::audit::record(&state, &actor, "scim.user.create", user.user_name.clone()).await;

            let mut body = match serde_json::to_value(&user) {
                Ok(v) => v,
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };
            match guardian.issue(&user.user_name, user.roles.clone(), None).await {
                Ok(plaintext) => {
                    body["urn:open-runo:params:scim:api-key"] = serde_json::Value::String(plaintext);
                    crate::audit::record(&state, "key-guardian", "key.auto_issue", user.user_name.clone()).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, owner = %user.user_name, "auto key issue failed");
                }
            }

            json_response(StatusCode::CREATED, &body)
        })
    })
}

/// GET /scim/v2/Users/:id — poem-free port of `handlers::scim::get_user`.
pub fn scim_get_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let id = params.get("id").unwrap_or("").to_string();
            match scim_user_store(&state).get(&id).await {
                Ok(Some(user)) => json_response(StatusCode::OK, &user),
                Ok(None) => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("user not found: {id}") }),
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// PUT /scim/v2/Users/:id — poem-free port of `handlers::scim::replace_user`,
/// including KeyGuardian's auto-revoke on deactivation.
pub fn scim_replace_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let id = params.get("id").unwrap_or("").to_string();
            let input: open_runo_scim::UserInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let user = match scim_user_store(&state).replace(&id, input).await {
                Ok(u) => u,
                Err(e) => {
                    let msg = e.to_string();
                    let status = if msg.contains("not found") {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::CONFLICT
                    };
                    return json_response(status, &serde_json::json!({ "error": msg }));
                }
            };

            crate::audit::record(&state, &actor, "scim.user.replace", user.user_name.clone()).await;

            if !user.active {
                if let Ok(n) = guardian.revoke_owner(&user.user_name).await {
                    if n > 0 {
                        crate::audit::record(
                            &state,
                            "key-guardian",
                            "key.auto_revoke",
                            format!("{} ({n} keys, deactivated)", user.user_name),
                        )
                        .await;
                    }
                }
            }

            json_response(StatusCode::OK, &user)
        })
    })
}

/// DELETE /scim/v2/Users/:id — poem-free port of `handlers::scim::delete_user`,
/// including KeyGuardian's auto-revoke on deletion.
pub fn scim_delete_user_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let id = params.get("id").unwrap_or("").to_string();

            let owner = match scim_user_store(&state).get(&id).await {
                Ok(u) => u.map(|u| u.user_name),
                Err(e) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &serde_json::json!({ "error": e.to_string() }),
                    )
                }
            };

            if let Err(e) = scim_user_store(&state).delete(&id).await {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                );
            }

            crate::audit::record(&state, &actor, "scim.user.delete", id).await;

            if let Some(owner) = owner {
                if let Ok(n) = guardian.revoke_owner(&owner).await {
                    if n > 0 {
                        crate::audit::record(
                            &state,
                            "key-guardian",
                            "key.auto_revoke",
                            format!("{owner} ({n} keys, deleted)"),
                        )
                        .await;
                    }
                }
            }

            empty_status(StatusCode::NO_CONTENT)
        })
    })
}

/// GET /scim/v2/Groups — poem-free port of `handlers::scim::list_groups`.
pub fn scim_list_groups_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let query = query_params(&req);
            let filter = query
                .get("filter")
                .and_then(|f| open_runo_scim::ScimGroupStore::parse_display_name_filter(f));

            match scim_group_store(&state).list(filter.as_deref()).await {
                Ok(groups) => json_response(
                    StatusCode::OK,
                    &open_runo_scim::GroupListResponse {
                        schemas: vec!["urn:ietf:params:scim:api:messages:2.0:ListResponse".into()],
                        total_results: groups.len(),
                        resources: groups,
                    },
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// POST /scim/v2/Groups — poem-free port of `handlers::scim::create_group`.
pub fn scim_create_group_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let input: open_runo_scim::GroupInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            match scim_group_store(&state).create(input).await {
                Ok(group) => {
                    crate::audit::record(&state, &actor, "scim.group.create", group.display_name.clone()).await;
                    json_response(StatusCode::CREATED, &group)
                }
                Err(e) => json_response(
                    StatusCode::CONFLICT,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// GET /scim/v2/Groups/:id — poem-free port of `handlers::scim::get_group`.
pub fn scim_get_group_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let id = params.get("id").unwrap_or("").to_string();
            match scim_group_store(&state).get(&id).await {
                Ok(Some(group)) => json_response(StatusCode::OK, &group),
                Ok(None) => json_response(
                    StatusCode::NOT_FOUND,
                    &serde_json::json!({ "error": format!("group not found: {id}") }),
                ),
                Err(e) => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                ),
            }
        })
    })
}

/// PUT /scim/v2/Groups/:id — poem-free port of `handlers::scim::replace_group`.
pub fn scim_replace_group_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let id = params.get("id").unwrap_or("").to_string();
            let input: open_runo_scim::GroupInput = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            match scim_group_store(&state).replace(&id, input).await {
                Ok(group) => {
                    crate::audit::record(&state, &actor, "scim.group.replace", group.display_name.clone()).await;
                    json_response(StatusCode::OK, &group)
                }
                Err(e) => {
                    let msg = e.to_string();
                    let status = if msg.contains("not found") {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::CONFLICT
                    };
                    json_response(status, &serde_json::json!({ "error": msg }))
                }
            }
        })
    })
}

/// DELETE /scim/v2/Groups/:id — poem-free port of `handlers::scim::delete_group`.
pub fn scim_delete_group_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            if let Err(status) = check_api_key(req.headers(), &guardian).await {
                return empty_status(status);
            }
            let actor = actor_from_headers(req.headers());
            let id = params.get("id").unwrap_or("").to_string();

            if let Err(e) = scim_group_store(&state).delete(&id).await {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "error": e.to_string() }),
                );
            }

            crate::audit::record(&state, &actor, "scim.group.delete", id).await;
            empty_status(StatusCode::NO_CONTENT)
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

    #[tokio::test]
    async fn persisted_query_register_and_fetch_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new()
            .route(
                Method::POST,
                "/api/persisted-queries",
                register_persisted_query_handler(Arc::clone(&state), Arc::clone(&guardian)),
            )
            .route(
                Method::GET,
                "/api/persisted-queries/:hash",
                get_persisted_query_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/persisted-queries"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "query": "{ health }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let hash = body["hash"].as_str().unwrap().to_string();
        assert_eq!(hash.len(), 64);

        let resp = client
            .get(format!("http://{addr}/api/persisted-queries/{hash}"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["query"], "{ health }");

        let resp = client
            .get(format!(
                "http://{addr}/api/persisted-queries/0000000000000000000000000000000000000000000000000000000000000000"
            ))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn register_persisted_query_requires_api_key() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/persisted-queries",
            register_persisted_query_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/persisted-queries"))
            .json(&serde_json::json!({ "query": "{ health }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn cache_purge_endpoints_respond() {
        use crate::middleware::html_cache::{HtmlCacheConfig, HtmlPageCache};

        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let cache = Arc::new(HtmlPageCache::new(HtmlCacheConfig::from_env()));
        let router = Router::new()
            .route(
                Method::POST,
                "/api/cache/purge",
                purge_page_handler(Arc::clone(&state), Arc::clone(&cache), Arc::clone(&guardian)),
            )
            .route(
                Method::POST,
                "/api/cache/purge-all",
                purge_all_pages_handler(Arc::clone(&state), Arc::clone(&cache), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/api/cache/purge"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "path": "/page/123" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["purged"], "/page/123");

        let resp = client
            .post(format!("http://{addr}/api/cache/purge-all"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn ai_stats_reports_disabled_when_no_predictor() {
        use crate::middleware::html_cache::{HtmlCacheConfig, HtmlPageCache};

        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let mut config = HtmlCacheConfig::from_env();
        config.ai = false;
        let cache = Arc::new(HtmlPageCache::new(config));
        let router = Router::new().route(Method::GET, "/api/cache/ai-stats", ai_stats_handler(cache, guardian));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/cache/ai-stats"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["ai_enabled"], false);
    }

    #[tokio::test]
    async fn integrity_check_endpoint_responds() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(
            Method::POST,
            "/api/integrity/check",
            integrity_check_handler(Arc::clone(&state), guardian),
        );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/integrity/check"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["healed"], 0);
    }

    #[tokio::test]
    async fn backup_export_and_import_roundtrip() {
        use crate::middleware::html_cache::{HtmlCacheConfig, HtmlPageCache};

        let state = Arc::new(AppState::new());
        state.schema_registry.lock().unwrap().register_in(
            "default",
            "bk",
            "type B { x: ID }",
            open_runo_schema_registry::Stage::Local,
        );

        let guardian = guardian(&state);
        let cache = Arc::new(HtmlPageCache::new(HtmlCacheConfig::from_env()));
        let router = Router::new()
            .route(
                Method::POST,
                "/api/backup/export",
                backup_export_handler(Arc::clone(&state), cache, Arc::clone(&guardian)),
            )
            .route(
                Method::POST,
                "/api/backup/import",
                backup_import_handler(Arc::clone(&state), guardian),
            );
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let dir = std::env::temp_dir().join(format!("orn-hyper-e2e-{}", uuid::Uuid::new_v4()));
        std::env::set_var("OPEN_RUNO_BACKUP_DIR", &dir);

        let resp = client
            .post(format!("http://{addr}/api/backup/export"))
            .header("x-api-key", "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let path = body["written"][0].as_str().unwrap().to_string();
        assert!(body["records"].as_u64().unwrap() >= 1);

        let resp = client
            .post(format!("http://{addr}/api/backup/import"))
            .header("x-api-key", "test-key")
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        std::env::remove_var("OPEN_RUNO_BACKUP_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scim_router(state: &Arc<AppState>, guardian: Arc<KeyGuardian>) -> Router {
        Router::new()
            .route(Method::GET, "/scim/v2/Users", scim_list_users_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::POST, "/scim/v2/Users", scim_create_user_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::GET, "/scim/v2/Users/:id", scim_get_user_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::PUT, "/scim/v2/Users/:id", scim_replace_user_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::DELETE, "/scim/v2/Users/:id", scim_delete_user_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::GET, "/scim/v2/Groups", scim_list_groups_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::POST, "/scim/v2/Groups", scim_create_group_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::GET, "/scim/v2/Groups/:id", scim_get_group_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::PUT, "/scim/v2/Groups/:id", scim_replace_group_handler(Arc::clone(state), Arc::clone(&guardian)))
            .route(Method::DELETE, "/scim/v2/Groups/:id", scim_delete_group_handler(Arc::clone(state), guardian))
    }

    #[tokio::test]
    async fn scim_user_lifecycle_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = scim_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();
        let key = "x-api-key";

        let resp = client
            .post(format!("http://{addr}/scim/v2/Users"))
            .header(key, "test-key")
            .json(&serde_json::json!({
                "userName": "alice@example.com",
                "displayName": "Alice",
                "emails": [{ "value": "alice@example.com", "primary": true }],
                "roles": ["developer"]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["userName"], "alice@example.com");
        assert_eq!(body["meta"]["resourceType"], "User");

        let issued = body["urn:open-runo:params:scim:api-key"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = client
            .post(format!("http://{addr}/scim/v2/Users"))
            .header(key, &issued)
            .json(&serde_json::json!({ "userName": "alice@example.com" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

        let resp = client
            .get(format!("http://{addr}/scim/v2/Users"))
            .query(&[("filter", r#"userName eq "alice@example.com""#)])
            .header(key, &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["totalResults"], 1);

        let resp = client
            .put(format!("http://{addr}/scim/v2/Users/{id}"))
            .header(key, &issued)
            .json(&serde_json::json!({ "userName": "alice@example.com", "active": false }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["active"], false);

        let resp = client
            .get(format!("http://{addr}/scim/v2/Users/{id}"))
            .header(key, &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn scim_group_lifecycle_roundtrip() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = scim_router(&state, guardian);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();
        let key = "x-api-key";

        let resp = client
            .post(format!("http://{addr}/scim/v2/Groups"))
            .header(key, "test-key")
            .json(&serde_json::json!({
                "displayName": "engineering",
                "members": [{ "value": "user-1", "display": "Alice" }]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["meta"]["resourceType"], "Group");

        let resp = client
            .put(format!("http://{addr}/scim/v2/Groups/{id}"))
            .header(key, "test-key")
            .json(&serde_json::json!({
                "displayName": "engineering",
                "members": [{ "value": "user-1" }, { "value": "user-2" }]
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["members"].as_array().unwrap().len(), 2);

        let resp = client
            .delete(format!("http://{addr}/scim/v2/Groups/{id}"))
            .header(key, "test-key")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn key_guardian_full_auto_lifecycle() {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = scim_router(&state, Arc::clone(&guardian))
            .route(Method::GET, "/api/db/status", db_status_handler(Arc::clone(&state), Arc::clone(&guardian)));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        client
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", "anything-goes")
            .send()
            .await
            .expect("request should succeed");

        let resp = client
            .post(format!("http://{addr}/scim/v2/Users"))
            .header("x-api-key", "bootstrap")
            .json(&serde_json::json!({ "userName": "eve@example.com", "roles": ["developer"] }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        let issued = body["urn:open-runo:params:scim:api-key"].as_str().unwrap().to_string();
        let user_id = body["id"].as_str().unwrap().to_string();
        assert!(issued.starts_with("orn_"));

        let resp = client
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", "anything-goes")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        let resp = client
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .delete(format!("http://{addr}/scim/v2/Users/{user_id}"))
            .header("x-api-key", &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

        let resp = client
            .get(format!("http://{addr}/api/db/status"))
            .header("x-api-key", &issued)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}

//! REST API client for open-runo-router, callable from WASM.
//!
//! Poem-free/Tauri-free equivalent of the old `src/api/client.ts`
//! `invoke()`-style helpers: plain async Rust functions that `fetch()` the
//! backend directly, decoding JSON via `serde`. No IPC bridge to a separate
//! host process — the WASM bundle and the API it calls are served by the
//! same `open-runo-router` binary, so this is a same-origin call.

use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

/// Dev-mode API key. `open-runo-router`'s KeyGuardian accepts any
/// non-empty key while its registry is empty (see `auth_hyper.rs`); a
/// real deployment would source this from a login flow instead.
const DEV_API_KEY: &str = "open-runo-desktop-wasm";

#[derive(Debug, Deserialize)]
pub struct Health {
    pub status: String,
    pub service: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct RegisterSchemaRequest<'a> {
    pub service_name: &'a str,
    pub sdl: &'a str,
    pub stage: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct RegisterSchemaResponse {
    pub id: String,
    pub namespace: String,
    pub service_name: String,
    pub stage: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct SchemaVersion {
    pub id: String,
    pub service_name: String,
    pub stage: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct SchemaHistoryResponse {
    pub versions: Vec<SchemaVersion>,
}

#[derive(Debug, Deserialize)]
pub struct FederationStatusResponse {
    pub contributing_services: Vec<String>,
    pub type_count: usize,
    pub field_count: usize,
}

#[derive(Debug, Serialize)]
pub struct AiRouteCandidate<'a> {
    pub provider: &'a str,
    pub estimated_cost_usd_per_1k_tokens: f64,
    pub estimated_latency_ms: u32,
    pub is_local: bool,
    pub context_length: u32,
}

#[derive(Debug, Serialize)]
pub struct AiRouteRequest<'a> {
    pub policy: &'a str,
    pub candidates: Vec<AiRouteCandidate<'a>>,
}

#[derive(Debug, Deserialize)]
pub struct AiRouteResponse {
    pub selected_provider: String,
    pub is_local: bool,
    pub estimated_cost_usd_per_1k_tokens: f64,
    pub estimated_latency_ms: u32,
}

/// Base URL for API calls. Empty string means same-origin (the WASM
/// bundle is served by the same open-runo-router binary it talks to).
fn base_url() -> &'static str {
    ""
}

async fn send(method: &str, path: &str, body: Option<&str>) -> Result<JsValue, String> {
    let opts = RequestInit::new();
    opts.set_method(method);
    opts.set_mode(RequestMode::SameOrigin);
    if let Some(body) = body {
        opts.set_body(&JsValue::from_str(body));
    }

    let url = format!("{}{path}", base_url());
    let request = Request::new_with_str_and_init(&url, &opts).map_err(|e| format!("{e:?}"))?;
    request
        .headers()
        .set("x-api-key", DEV_API_KEY)
        .map_err(|e| format!("{e:?}"))?;
    if body.is_some() {
        request
            .headers()
            .set("content-type", "application/json")
            .map_err(|e| format!("{e:?}"))?;
    }

    let window = web_sys::window().ok_or("no window")?;
    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("fetch error: {e:?}"))?;
    let resp: Response = resp_value.dyn_into().map_err(|e| format!("{e:?}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    JsFuture::from(resp.json().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("body read error: {e:?}"))
}

async fn get_json<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T, String> {
    let json = send("GET", path, None).await?;
    serde_wasm_bindgen::from_value(json).map_err(|e| format!("decode error: {e}"))
}

async fn post_json<T: for<'de> Deserialize<'de>>(
    path: &str,
    body: &impl Serialize,
) -> Result<T, String> {
    let body = serde_json::to_string(body).map_err(|e| format!("encode error: {e}"))?;
    let json = send("POST", path, Some(&body)).await?;
    serde_wasm_bindgen::from_value(json).map_err(|e| format!("decode error: {e}"))
}

pub async fn health_check() -> Result<Health, String> {
    get_json::<Health>("/health").await
}

pub async fn register_schema(
    service_name: &str,
    sdl: &str,
    stage: &str,
) -> Result<RegisterSchemaResponse, String> {
    post_json(
        "/api/schemas",
        &RegisterSchemaRequest { service_name, sdl, stage },
    )
    .await
}

pub async fn get_schema_history(service: &str) -> Result<SchemaHistoryResponse, String> {
    get_json(&format!("/api/schemas/{service}/history")).await
}

pub async fn federation_status() -> Result<FederationStatusResponse, String> {
    get_json("/api/federation/status").await
}

pub async fn ai_route(
    policy: &str,
    candidates: Vec<AiRouteCandidate<'_>>,
) -> Result<AiRouteResponse, String> {
    post_json("/api/ai/route", &AiRouteRequest { policy, candidates }).await
}

#[derive(Debug, Deserialize)]
pub struct DbRecordItem {
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct DbRecordListResponse {
    pub count: usize,
    pub records: Vec<DbRecordItem>,
}

#[derive(Debug, Deserialize)]
pub struct DbRecordResponse {
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct DbUpsertRequest<'a> {
    value: &'a serde_json::Value,
}

pub async fn db_list(table: &str) -> Result<DbRecordListResponse, String> {
    get_json(&format!("/api/db/{table}")).await
}

pub async fn db_get(table: &str, key: &str) -> Result<DbRecordResponse, String> {
    get_json(&format!("/api/db/{table}/{key}")).await
}

pub async fn db_put(table: &str, key: &str, value_json: &str) -> Result<DbRecordResponse, String> {
    let value: serde_json::Value =
        serde_json::from_str(value_json).map_err(|e| format!("invalid JSON value: {e}"))?;
    let body = serde_json::to_string(&DbUpsertRequest { value: &value })
        .map_err(|e| format!("encode error: {e}"))?;
    let json = send("PUT", &format!("/api/db/{table}/{key}"), Some(&body)).await?;
    serde_wasm_bindgen::from_value(json).map_err(|e| format!("decode error: {e}"))
}

pub async fn db_delete(table: &str, key: &str) -> Result<(), String> {
    send("DELETE", &format!("/api/db/{table}/{key}"), None).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ScimUser {
    pub id: String,
    #[serde(rename = "userName")]
    pub user_name: String,
    pub active: bool,
    pub roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ScimUserList {
    #[serde(rename = "totalResults")]
    pub total_results: usize,
    #[serde(rename = "Resources")]
    pub resources: Vec<ScimUser>,
}

#[derive(Debug, Serialize)]
pub struct ScimCreateUserRequest<'a> {
    #[serde(rename = "userName")]
    pub user_name: &'a str,
    pub roles: Vec<&'a str>,
}

pub async fn scim_list_users() -> Result<ScimUserList, String> {
    get_json("/scim/v2/Users").await
}

pub async fn scim_create_user(user_name: &str, roles: Vec<&str>) -> Result<serde_json::Value, String> {
    post_json("/scim/v2/Users", &ScimCreateUserRequest { user_name, roles }).await
}

pub async fn scim_delete_user(id: &str) -> Result<(), String> {
    send("DELETE", &format!("/scim/v2/Users/{id}"), None).await?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct PqRegisterRequest<'a> {
    query: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct PqRegisterResponse {
    pub hash: String,
    pub registered_at: String,
}

#[derive(Debug, Deserialize)]
pub struct PqQueryResponse {
    pub hash: String,
    pub query: String,
    pub registered_at: String,
}

pub async fn register_persisted_query(query: &str) -> Result<PqRegisterResponse, String> {
    post_json("/api/persisted-queries", &PqRegisterRequest { query }).await
}

pub async fn get_persisted_query(hash: &str) -> Result<PqQueryResponse, String> {
    get_json(&format!("/api/persisted-queries/{hash}")).await
}

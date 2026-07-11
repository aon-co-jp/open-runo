//! REST API client for open-runo-router, callable from WASM.
//!
//! Poem-free/Tauri-free equivalent of the old `src/api/client.ts`
//! `invoke()`-style helpers: plain async Rust functions that `fetch()` the
//! backend directly, decoding JSON via `serde`. No IPC bridge to a separate
//! host process — the WASM bundle and the API it calls are served by the
//! same `open-runo-router` binary, so this is a same-origin call.

use open_runo_api_types::{
    DbRecordListResponse, DbRecordResponse, DbUpsertRequest, FederationStatusResponse, RateLimitedResponse,
    RegisterSchemaRequest, SchemaHistoryResponse, SchemaVersion,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

/// `localStorage` key the self-issued API key is cached under, so a page
/// reload doesn't re-issue a fresh one every time.
const STORAGE_KEY: &str = "open-runo-api-key";

thread_local! {
    /// In-memory cache for this page load. WASM is single-threaded so a
    /// `RefCell` is enough — no need for a `Mutex`.
    static CACHED_KEY: RefCell<Option<String>> = const { RefCell::new(None) };
}

#[derive(Debug, Deserialize)]
struct SelfIssueResponse {
    api_key: String,
}

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// Get a working API key, obtaining one automatically if needed — the user
/// of the app never sees, enters, or configures a key. Order: in-memory
/// cache → `localStorage` (survives reloads) → `POST /api/keys/self-issue`
/// (no auth required, see `handlers_hyper::self_issue_key_handler`).
/// The obtained key is cached both in memory and in `localStorage`.
async fn get_or_issue_api_key() -> Result<String, String> {
    if let Some(key) = CACHED_KEY.with(|c| c.borrow().clone()) {
        return Ok(key);
    }
    if let Some(storage) = local_storage() {
        if let Ok(Some(key)) = storage.get_item(STORAGE_KEY) {
            if !key.is_empty() {
                CACHED_KEY.with(|c| *c.borrow_mut() = Some(key.clone()));
                return Ok(key);
            }
        }
    }

    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(RequestMode::SameOrigin);
    let request =
        Request::new_with_str_and_init(&format!("{}/api/keys/self-issue", base_url()), &opts)
            .map_err(|e| format!("{e:?}"))?;
    let window = web_sys::window().ok_or("no window")?;
    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("self-issue fetch error: {e:?}"))?;
    let resp: Response = resp_value.dyn_into().map_err(|e| format!("{e:?}"))?;
    if !resp.ok() {
        return Err(format!("self-issue failed: HTTP {}", resp.status()));
    }
    let json = JsFuture::from(resp.json().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("self-issue body read error: {e:?}"))?;
    let parsed: SelfIssueResponse =
        serde_wasm_bindgen::from_value(json).map_err(|e| format!("self-issue decode error: {e}"))?;

    CACHED_KEY.with(|c| *c.borrow_mut() = Some(parsed.api_key.clone()));
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(STORAGE_KEY, &parsed.api_key);
    }
    Ok(parsed.api_key)
}

#[derive(Debug, Deserialize)]
pub struct Health {
    pub status: String,
    pub service: String,
    pub version: String,
}

// RegisterSchemaRequest/SchemaVersion/SchemaHistoryResponse/
// FederationStatusResponse now live in open_runo_api_types (imported
// above) -- shared with open-runo-router and open-runo-cli so the wire
// shape can't drift between them again (see CLAUDE.md HANDOFF, 2026-07-11).

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

async fn do_fetch(method: &str, path: &str, body: Option<&str>, api_key: &str) -> Result<Response, String> {
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
        .set("x-api-key", api_key)
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
    resp_value.dyn_into().map_err(|e| format!("{e:?}"))
}

/// Drop the cached key (memory + `localStorage`) so the next call
/// transparently self-issues a fresh one.
fn clear_cached_api_key() {
    CACHED_KEY.with(|c| *c.borrow_mut() = None);
    if let Some(storage) = local_storage() {
        let _ = storage.remove_item(STORAGE_KEY);
    }
}

async fn send(method: &str, path: &str, body: Option<&str>) -> Result<JsValue, String> {
    let api_key = get_or_issue_api_key().await?;
    let mut resp = do_fetch(method, path, body, &api_key).await?;

    // The cached key may have expired or been revoked server-side (24h TTL,
    // see self_issue_key_handler). Transparently self-issue a new one and
    // retry once, rather than surfacing a confusing 401 to the UI.
    if resp.status() == 401 {
        clear_cached_api_key();
        let fresh_key = get_or_issue_api_key().await?;
        resp = do_fetch(method, path, body, &fresh_key).await?;
    }

    if !resp.ok() {
        // The server tags every response with an X-Request-Id (see
        // open_runo_router::middleware_hyper::with_tracing); surfacing it
        // in the error lets a user hand a specific ID to whoever reads
        // the server logs, instead of describing "it just failed".
        let request_id = resp.headers().get("x-request-id").ok().flatten();
        let suffix = request_id.map(|id| format!(" (request-id: {id})")).unwrap_or_default();

        if resp.status() == 429 {
            if let Ok(promise) = resp.json() {
                if let Ok(json) = JsFuture::from(promise).await {
                    if let Ok(body) = serde_wasm_bindgen::from_value::<RateLimitedResponse>(json) {
                        return Err(format!("rate limited, retry in {}s{suffix}", body.retry_after_secs));
                    }
                }
            }
            return Err(format!("HTTP 429 (rate limited){suffix}"));
        }

        return Err(format!("HTTP {}{suffix}", resp.status()));
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

pub async fn register_schema(service_name: &str, sdl: &str, stage: &str) -> Result<SchemaVersion, String> {
    post_json(
        "/api/schemas",
        &RegisterSchemaRequest {
            service_name: service_name.to_string(),
            sdl: sdl.to_string(),
            stage: stage.to_string(),
            namespace: None,
        },
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

// DbRecordListResponse/DbRecordResponse/DbUpsertRequest now live in
// open_runo_api_types (imported above) -- the frontend's previous copies
// of the response types both silently omitted the `table` field the
// router actually sends (see CLAUDE.md HANDOFF, 2026-07-11).

pub async fn db_list(table: &str) -> Result<DbRecordListResponse, String> {
    get_json(&format!("/api/db/{table}")).await
}

pub async fn db_get(table: &str, key: &str) -> Result<DbRecordResponse, String> {
    get_json(&format!("/api/db/{table}/{key}")).await
}

pub async fn db_put(table: &str, key: &str, value_json: &str) -> Result<DbRecordResponse, String> {
    let value: serde_json::Value =
        serde_json::from_str(value_json).map_err(|e| format!("invalid JSON value: {e}"))?;
    let body = serde_json::to_string(&DbUpsertRequest { value })
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

#[derive(Debug, Serialize)]
struct PurgeRequest<'a> {
    path: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct PurgeResponse {
    pub purged: String,
}

#[derive(Debug, Deserialize)]
pub struct AiStatsResponse {
    pub ai_enabled: bool,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_ratio: f64,
    pub tracked_keys: usize,
}

pub async fn cache_purge(path: &str) -> Result<PurgeResponse, String> {
    post_json("/api/cache/purge", &PurgeRequest { path }).await
}

pub async fn cache_purge_all() -> Result<PurgeResponse, String> {
    post_json("/api/cache/purge-all", &serde_json::json!({})).await
}

pub async fn cache_ai_stats() -> Result<AiStatsResponse, String> {
    get_json("/api/cache/ai-stats").await
}

#[derive(Debug, Deserialize)]
pub struct ExportResponse {
    pub written: Vec<String>,
    pub records: usize,
}

#[derive(Debug, Deserialize)]
pub struct IntegrityResponse {
    pub backend: String,
    pub healed: usize,
}

pub async fn backup_export() -> Result<ExportResponse, String> {
    post_json("/api/backup/export", &serde_json::json!({})).await
}

pub async fn integrity_check() -> Result<IntegrityResponse, String> {
    post_json("/api/integrity/check", &serde_json::json!({})).await
}

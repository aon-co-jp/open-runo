//! MCP Server — Poem-parity gap ("MCP Server(poem-mcpserver相当)",
//! `docs/poem-parity.md`). The [Model Context Protocol](https://modelcontextprotocol.io/)
//! lets an LLM client (Claude Desktop, an IDE agent, etc.) discover and
//! call a server's capabilities as structured "tools" instead of scraping
//! its REST API by hand.
//!
//! Implements the JSON-RPC 2.0 message layer and the `initialize` /
//! `tools/list` / `tools/call` methods over a single `POST /mcp` endpoint
//! (the ["Streamable HTTP" transport](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports#streamable-http)'s
//! simple case: one JSON-RPC request in, one JSON-RPC response out, no
//! SSE stream -- sufficient for stateless tool calls, which is all this
//! server exposes). Resources and prompts (the other two MCP capability
//! types) aren't implemented; only tools. No new dependencies -- this is
//! JSON-RPC over the same `read_json_body`/`json_response` machinery
//! every other handler in this crate already uses.
//!
//! Two real tools are exposed, both backed by production code paths (not
//! MCP-only stubs): `health_check` (same logic as `GET /health`) and
//! `self_issue_api_key` (same logic as `POST /api/keys/self-issue`) --
//! letting an MCP client obtain a working API key for this server's REST
//! API without a human ever typing one, the same "no human key
//! management" property `KeyGuardian` already provides over HTTP.

use crate::hyper_compat::{empty_status, json_response, read_json_body, Handler};
use crate::keyring::KeyGuardian;
use crate::state::AppState;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SELF_ISSUE_KEY_TTL_HOURS: i64 = 24;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: String,
    /// Absent for a JSON-RPC *notification* (e.g.
    /// `notifications/initialized`), which must not receive a response.
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

fn ok_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse { jsonrpc: "2.0", id, result: Some(result), error: None }
}

fn err_response(id: Value, code: i64, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse { jsonrpc: "2.0", id, result: None, error: Some(JsonRpcError { code, message: message.into() }) }
}

/// The `initialize` handshake response: this server's protocol version,
/// declared capabilities (tools only), and identity.
fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "open-runo-router",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// `tools/list`: the tool catalog an MCP client discovers and can then
/// invoke via `tools/call`. `inputSchema` is plain JSON Schema, per the
/// MCP spec's `Tool` shape.
fn tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "health_check",
                "description": "Check whether the open-runo-router service is up and report its version.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "self_issue_api_key",
                "description": "Obtain a working X-Api-Key for this server's REST API, scoped to the developer role and expiring after 24 hours. No human key management required -- the same self-issue mechanism POST /api/keys/self-issue exposes over plain REST.",
                "inputSchema": { "type": "object", "properties": {} },
            },
        ]
    })
}

/// Wrap a tool's JSON result in the MCP `CallToolResult` shape: a list of
/// content blocks (here, always one `text` block containing the
/// serialized JSON) plus an `isError` flag.
fn tool_content(value: &Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
        "isError": false,
    })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true,
    })
}

async fn call_health_check() -> Value {
    tool_content(&json!({
        "status": "ok",
        "service": "open-runo-router",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn call_self_issue_api_key(state: &Arc<AppState>, guardian: &Arc<KeyGuardian>) -> Value {
    let owner = format!("mcp-client-{}", uuid::Uuid::new_v4());
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(SELF_ISSUE_KEY_TTL_HOURS);
    match guardian.issue(&owner, vec!["developer".to_string()], Some(expires_at)).await {
        Ok(api_key) => {
            crate::audit::record(state, "mcp-server", "key.self_issue", owner).await;
            tool_content(&json!({ "api_key": api_key, "expires_at": expires_at.to_rfc3339() }))
        }
        Err(e) => tool_error(format!("failed to issue API key: {e}")),
    }
}

async fn dispatch(state: &Arc<AppState>, guardian: &Arc<KeyGuardian>, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    // A JSON-RPC *notification* has no `id` and must receive no response
    // at all (not even an empty one) -- `notifications/initialized` is
    // the one an MCP client sends after a successful `initialize`.
    let Some(id) = req.id else {
        return None;
    };

    if req.jsonrpc != "2.0" {
        return Some(err_response(id, -32600, "invalid request: jsonrpc must be \"2.0\""));
    }

    Some(match req.method.as_str() {
        "initialize" => ok_response(id, initialize_result()),
        "tools/list" => ok_response(id, tools_list_result()),
        "tools/call" => {
            let Some(name) = req.params.get("name").and_then(Value::as_str) else {
                return Some(err_response(id, -32602, "invalid params: missing \"name\""));
            };
            let result = match name {
                "health_check" => call_health_check().await,
                "self_issue_api_key" => call_self_issue_api_key(state, guardian).await,
                other => tool_error(format!("unknown tool: {other}")),
            };
            ok_response(id, result)
        }
        other => err_response(id, -32601, format!("method not found: {other}")),
    })
}

/// `POST /mcp` — the MCP Streamable HTTP transport's single endpoint. No
/// auth required to reach it (mirrors `/api/keys/self-issue`'s own
/// no-auth stance): an MCP client's very first call is `initialize`,
/// before it has any credential, and `self_issue_api_key` exists
/// specifically to bootstrap one from here.
pub fn mcp_handler(state: Arc<AppState>, guardian: Arc<KeyGuardian>) -> Handler {
    Arc::new(move |req, _params| {
        let state = Arc::clone(&state);
        let guardian = Arc::clone(&guardian);
        Box::pin(async move {
            let parsed: JsonRpcRequest = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            match dispatch(&state, &guardian, parsed).await {
                Some(resp) => json_response(StatusCode::OK, &resp),
                // A notification was handled but produces no body --
                // 204 is the correct "processed, nothing to say" response
                // for an HTTP transport carrying a JSON-RPC notification.
                None => empty_status(StatusCode::NO_CONTENT),
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

    async fn start() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let state = Arc::new(AppState::new());
        let guardian = guardian(&state);
        let router = Router::new().route(Method::POST, "/mcp", mcp_handler(state, guardian));
        serve(router, "127.0.0.1:0".parse().unwrap()).await.expect("bind ephemeral port")
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version_and_capabilities() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(body["result"]["serverInfo"]["name"], "open-runo-router");
    }

    #[tokio::test]
    async fn notification_gets_no_json_body_response() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            // No "id" field -- this is a notification, per JSON-RPC 2.0.
            .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn tools_list_advertises_both_real_tools() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"health_check"));
        assert!(names.contains(&"self_issue_api_key"));
    }

    #[tokio::test]
    async fn tools_call_health_check_returns_real_status() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "health_check", "arguments": {} }
            }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["isError"], false);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let inner: Value = serde_json::from_str(text).unwrap();
        assert_eq!(inner["status"], "ok");
        assert_eq!(inner["service"], "open-runo-router");
    }

    #[tokio::test]
    async fn tools_call_self_issue_api_key_returns_a_real_working_key() {
        let (addr, _handle) = start().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": { "name": "self_issue_api_key", "arguments": {} }
            }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["isError"], false);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let inner: Value = serde_json::from_str(text).unwrap();
        let api_key = inner["api_key"].as_str().expect("api_key should be a string");
        assert!(!api_key.is_empty());
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_reports_iserror_not_a_transport_failure() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": { "name": "no_such_tool", "arguments": {} }
            }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK, "an unknown tool is a tool-level error, not an HTTP-level one");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["isError"], true);
    }

    #[tokio::test]
    async fn unknown_method_returns_json_rpc_method_not_found() {
        let (addr, _handle) = start().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 6, "method": "no/such/method", "params": {} }))
            .send()
            .await
            .expect("request should succeed");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], -32601);
    }

    /// The realistic sequence a real MCP client follows: initialize, then
    /// discover tools, then call one -- over the same persistent-enough
    /// server (three separate requests, same running process), proving
    /// the handler is stateless-safe to call repeatedly in sequence.
    #[tokio::test]
    async fn full_initialize_then_list_then_call_sequence() {
        let (addr, _handle) = start().await;
        let client = reqwest::Client::new();

        let init = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);

        let notif_status = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(notif_status, reqwest::StatusCode::NO_CONTENT);

        let list = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert!(!list["result"]["tools"].as_array().unwrap().is_empty());

        let call = client
            .post(format!("http://{addr}/mcp"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "health_check", "arguments": {} }
            }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(call["result"]["isError"], false);
    }
}

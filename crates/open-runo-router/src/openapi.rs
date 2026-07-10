//! OpenAPI 3.0 spec for the REST admin surface.
//!
//! Poem itself ships a `poem-openapi` crate that auto-generates specs from
//! typed handlers via macros; this crate doesn't depend on Poem, so this
//! is a hand-written equivalent — a static `serde_json::Value` describing
//! every REST path, matching the endpoint table in `lib.rs`'s doc comment.
//! It gives the same practical benefit (import into Postman/Insomnia,
//! generate client SDKs, browse in any Swagger UI) without macro machinery.
//! GraphQL already has interactive docs via GraphiQL (`GET /graphql`);
//! this closes the equivalent gap for the REST side.

use crate::hyper_compat::{json_response, Handler};
use hyper::StatusCode;
use serde_json::{json, Value};
use std::sync::Arc;

fn api_key_security() -> Value {
    json!([{ "ApiKeyAuth": [] }])
}

fn spec() -> Value {
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "open-runo REST API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST admin surface for open-runo-router. GraphQL Federation is served separately at POST /graphql (GraphiQL at GET /graphql)."
        },
        "components": {
            "securitySchemes": {
                "ApiKeyAuth": {
                    "type": "apiKey",
                    "in": "header",
                    "name": "X-Api-Key"
                }
            }
        },
        "paths": {
            "/health": {
                "get": { "summary": "Service health check", "security": [], "responses": { "200": { "description": "OK" } } }
            },
            "/healthz": {
                "get": { "summary": "Kubernetes-style health alias", "security": [], "responses": { "200": { "description": "OK" } } }
            },
            "/api/schemas": {
                "post": { "summary": "Register a schema version", "security": api_key_security(), "responses": { "200": { "description": "Registered" }, "422": { "description": "Validation failed" } } }
            },
            "/api/schemas/{service}": {
                "get": { "summary": "Latest schema for a service", "security": api_key_security(), "parameters": [{ "name": "service", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" }, "404": { "description": "Not found" } } }
            },
            "/api/schemas/{service}/history": {
                "get": { "summary": "Full schema version history", "security": api_key_security(), "parameters": [{ "name": "service", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } }
            },
            "/api/federation/compose": {
                "post": { "summary": "Compose service schemas into a federated graph", "security": api_key_security(), "responses": { "200": { "description": "Composed" } } }
            },
            "/api/federation/status": {
                "get": { "summary": "Current composed schema summary", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/ai/route": {
                "post": { "summary": "Select the best AI provider for a request", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/db/status": {
                "get": { "summary": "DUAL DATABASE backend name & health", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/db/routing": {
                "get": { "summary": "Per-table routing decisions", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/db/{table}": {
                "get": { "summary": "List all records in a table", "security": api_key_security(), "parameters": [{ "name": "table", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } }
            },
            "/api/db/{table}/{key}": {
                "get": { "summary": "Get one record", "security": api_key_security(), "parameters": [{ "name": "table", "in": "path", "required": true, "schema": { "type": "string" } }, { "name": "key", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" }, "404": { "description": "Not found" } } },
                "put": { "summary": "Upsert a record", "security": api_key_security(), "parameters": [{ "name": "table", "in": "path", "required": true, "schema": { "type": "string" } }, { "name": "key", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "Saved" } } },
                "delete": { "summary": "Delete a record", "security": api_key_security(), "parameters": [{ "name": "table", "in": "path", "required": true, "schema": { "type": "string" } }, { "name": "key", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "Deleted" } } }
            },
            "/api/cache/purge": {
                "post": { "summary": "Purge one HTML page from the cache", "security": api_key_security(), "responses": { "200": { "description": "Purged" } } }
            },
            "/api/cache/purge-all": {
                "post": { "summary": "Purge the entire HTML page cache", "security": api_key_security(), "responses": { "200": { "description": "Purged" } } }
            },
            "/api/cache/ai-stats": {
                "get": { "summary": "Self-learning cache predictor stats", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/backup/export": {
                "post": { "summary": "Export a portable backup", "security": api_key_security(), "responses": { "200": { "description": "Exported" } } }
            },
            "/api/backup/import": {
                "post": { "summary": "Import a portable backup", "security": api_key_security(), "responses": { "200": { "description": "Imported" } } }
            },
            "/api/backup/restore-latest": {
                "post": { "summary": "Restore from the newest available backup", "security": api_key_security(), "responses": { "200": { "description": "Restored" }, "404": { "description": "No backup found" } } }
            },
            "/api/migrate/export-sql": {
                "post": { "summary": "SQL dump for engine migration (postgres/mysql/generic)", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/migrate/export-csv": {
                "post": { "summary": "CSV export for BI/Snowflake ingestion", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/integrity/check": {
                "post": { "summary": "Two-database reconciliation, self-healing", "security": api_key_security(), "responses": { "200": { "description": "OK" } } }
            },
            "/api/persisted-queries": {
                "post": { "summary": "Register a GraphQL document (Trusted Documents)", "security": api_key_security(), "responses": { "200": { "description": "Registered" } } }
            },
            "/api/persisted-queries/{hash}": {
                "get": { "summary": "Fetch a registered document by hash", "security": api_key_security(), "parameters": [{ "name": "hash", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" }, "404": { "description": "Not found" } } }
            },
            "/scim/v2/Users": {
                "get": { "summary": "List SCIM users (RFC 7644)", "security": api_key_security(), "responses": { "200": { "description": "OK" } } },
                "post": { "summary": "Create a SCIM user (auto-issues an API key)", "security": api_key_security(), "responses": { "201": { "description": "Created" }, "409": { "description": "userName already exists" } } }
            },
            "/scim/v2/Users/{id}": {
                "get": { "summary": "Fetch a SCIM user", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } },
                "put": { "summary": "Replace a SCIM user (deactivation auto-revokes keys)", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } },
                "delete": { "summary": "Delete a SCIM user (auto-revokes keys)", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "204": { "description": "Deleted" } } }
            },
            "/scim/v2/Groups": {
                "get": { "summary": "List SCIM groups", "security": api_key_security(), "responses": { "200": { "description": "OK" } } },
                "post": { "summary": "Create a SCIM group", "security": api_key_security(), "responses": { "201": { "description": "Created" } } }
            },
            "/scim/v2/Groups/{id}": {
                "get": { "summary": "Fetch a SCIM group", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } },
                "put": { "summary": "Replace a SCIM group's membership", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "OK" } } },
                "delete": { "summary": "Delete a SCIM group", "security": api_key_security(), "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "204": { "description": "Deleted" } } }
            },
            "/api/events": {
                "get": { "summary": "Server-Sent Events stream of schema/federation changes", "security": api_key_security(), "responses": { "200": { "description": "text/event-stream" } } }
            }
        }
    })
}

/// `GET /api/openapi.json` — serves the static OpenAPI 3.0 document above.
/// No auth required (matches `/health`'s exemption — the spec itself
/// contains no data, just endpoint shapes).
pub fn openapi_handler() -> Handler {
    Arc::new(move |_req, _params| Box::pin(async move { json_response(StatusCode::OK, &spec()) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyper_compat::{serve, Router};
    use hyper::Method;

    #[tokio::test]
    async fn openapi_spec_is_valid_json_and_lists_known_paths() {
        let router = Router::new().route(Method::GET, "/api/openapi.json", openapi_handler());
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/openapi.json"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.expect("valid json body");
        assert_eq!(body["openapi"], "3.0.3");
        assert!(body["paths"]["/health"].is_object());
        assert!(body["paths"]["/api/schemas"]["post"].is_object());
        assert!(body["paths"]["/scim/v2/Users/{id}"]["delete"].is_object());
        assert_eq!(
            body["components"]["securitySchemes"]["ApiKeyAuth"]["name"],
            "X-Api-Key"
        );
    }
}

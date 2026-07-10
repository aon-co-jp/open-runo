//! Poem-free `/graphql` endpoint, built on `open_runo_router::hyper_compat`
//! instead of `async_graphql_poem`. `async-graphql` itself has no Poem
//! dependency — only its poem integration crate did — so this is a
//! straight port of `graphql_handler`/`graphiql` from `lib.rs` onto plain
//! hyper request/response types.
//!
//! **Scope note**: GraphQL Subscriptions over WebSocket (`/graphql/ws`,
//! the `graphql-ws` protocol) are not ported here yet — hyper's raw
//! `Upgrade` handling for WebSocket is a separate, larger piece of work.
//! `graphql_route` (poem-based, still available in `lib.rs`) remains the
//! only way to get subscriptions until that lands.

use crate::{
    cache_key, is_cacheable_query, persisted_query_hash, CacheConfig, GatewayCache,
    OpenRunoSchema,
};
use open_runo_cache::InMemoryTtlCache;
use open_runo_persisted_queries::{EnforcementMode, PersistedQueryStore};
use open_runo_router::hyper_compat::{html_response, json_response, read_json_body, Handler};
use open_runo_router::state::AppState;
use std::sync::Arc;

/// `GET /graphql` — the GraphiQL playground (static HTML, no auth).
pub fn graphiql_handler() -> Handler {
    Arc::new(move |_req, _params| {
        Box::pin(async move {
            let html = async_graphql::http::GraphiQLSource::build()
                .endpoint("/graphql")
                .finish();
            html_response(hyper::StatusCode::OK, html)
        })
    })
}

/// `POST /graphql` — poem-free port of `graphql_handler`. Same persisted-
/// query resolution and response-cache behavior as the poem version.
pub fn graphql_post_handler(
    schema: OpenRunoSchema,
    store: Arc<PersistedQueryStore>,
    cache: Arc<GatewayCache>,
) -> Handler {
    Arc::new(move |req, _params| {
        let schema = schema.clone();
        let store = Arc::clone(&store);
        let cache = Arc::clone(&cache);
        Box::pin(async move {
            let mut request: async_graphql::Request = match read_json_body(req).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };

            let hash = persisted_query_hash(&request);
            let raw = if request.query.trim().is_empty() {
                None
            } else {
                Some(request.query.clone())
            };

            if hash.is_some() || store.mode() == EnforcementMode::Enforce {
                match store.resolve(hash.as_deref(), raw.as_deref()).await {
                    Ok(document) => request.query = document,
                    Err(e) => {
                        let resp = async_graphql::Response::from_errors(vec![
                            async_graphql::ServerError::new(e.to_string(), None),
                        ]);
                        return json_response(hyper::StatusCode::OK, &resp);
                    }
                }
            }

            let use_cache = cache.config.enabled && is_cacheable_query(&request.query);
            let key = if use_cache { Some(cache_key(&request)) } else { None };

            if let Some(key) = &key {
                if let Ok(Some(cached)) = cache.store.get(key).await {
                    if let Ok(response) = serde_json::from_str::<async_graphql::Response>(&cached) {
                        return json_response(hyper::StatusCode::OK, &response);
                    }
                }
            }

            let response = schema.execute(request).await;

            if let Some(key) = &key {
                if response.is_ok() {
                    if let Ok(serialized) = serde_json::to_string(&response) {
                        let _ = cache.store.set(key, &serialized, cache.config.ttl).await;
                    }
                }
            }

            json_response(hyper::StatusCode::OK, &response)
        })
    })
}

/// Build the poem-free `/graphql` GET+POST handlers, pre-wired with
/// `state`. Register both on a `hyper_compat::Router` at the same path
/// (the router dispatches by method, so `GET /graphql` → GraphiQL and
/// `POST /graphql` → query execution can share one path string).
pub fn graphql_handlers(state: Arc<AppState>) -> (Handler, Handler) {
    let schema = crate::build_schema(Arc::clone(&state));
    let store = Arc::new(PersistedQueryStore::new(
        Arc::clone(&state.db),
        crate::pq_mode_from_env(),
    ));
    let cache = Arc::new(GatewayCache {
        store: Arc::new(InMemoryTtlCache::new()),
        config: CacheConfig::from_env(),
    });
    (graphiql_handler(), graphql_post_handler(schema, store, cache))
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_runo_router::hyper_compat::{serve, Router};

    #[tokio::test]
    async fn graphiql_serves_html() {
        let state = Arc::new(AppState::new());
        let (get_h, _post_h) = graphql_handlers(state);
        let router = Router::new().route(hyper::Method::GET, "/graphql", get_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/graphql"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        let body = resp.text().await.unwrap();
        assert!(body.contains("graphiql") || body.contains("GraphiQL"));
    }

    #[tokio::test]
    async fn graphql_post_executes_health_query() {
        let state = Arc::new(AppState::new());
        let (_get_h, post_h) = graphql_handlers(state);
        let router = Router::new().route(hyper::Method::POST, "/graphql", post_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/graphql"))
            .json(&serde_json::json!({ "query": "{ health }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["data"]["health"], "ok");
    }

    #[tokio::test]
    async fn graphql_post_reports_query_errors() {
        let state = Arc::new(AppState::new());
        let (_get_h, post_h) = graphql_handlers(state);
        let router = Router::new().route(hyper::Method::POST, "/graphql", post_h);
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/graphql"))
            .json(&serde_json::json!({ "query": "{ notAField }" }))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert!(body["errors"].as_array().is_some_and(|e| !e.is_empty()));
    }
}

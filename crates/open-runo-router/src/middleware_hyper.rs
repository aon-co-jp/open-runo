//! Poem-free middleware for the hyper_compat stack: plain functions that
//! wrap a [`Handler`] and return a new [`Handler`] ("function in, function
//! out" composition — see `hyper_compat.rs` module doc), rather than
//! reimplementing `poem::Middleware`/`Endpoint` traits.

use crate::hyper_compat::{empty_status, Handler, Response};
use hyper::StatusCode;
use open_runo_security::RateLimiter;
use std::env;
use std::sync::Arc;

/// Wrap `inner` so every response gets CORS headers, and `OPTIONS`
/// preflight requests are answered directly without reaching `inner`.
/// Poem-free port of `middleware::cors::build_cors`'s behavior.
pub fn with_cors(inner: Handler) -> Handler {
    let origins = env::var("OPEN_RUNO_CORS_ALLOWED_ORIGINS").unwrap_or_default();
    let allowed: Vec<String> = origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let allow_origin_header = req
            .headers()
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .map(|origin| {
                if allowed.is_empty() || allowed.iter().any(|a| a == origin) {
                    origin.to_string()
                } else {
                    "null".to_string()
                }
            })
            .unwrap_or_else(|| "*".to_string());
        let is_preflight = req.method() == hyper::Method::OPTIONS;

        Box::pin(async move {
            let mut resp: Response = if is_preflight {
                empty_status(StatusCode::OK)
            } else {
                inner(req, params).await
            };
            let headers = resp.headers_mut();
            headers.insert(
                "access-control-allow-origin",
                allow_origin_header.parse().unwrap_or_else(|_| "*".parse().unwrap()),
            );
            headers.insert(
                "access-control-allow-methods",
                "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
            );
            headers.insert(
                "access-control-allow-headers",
                "x-api-key, authorization, content-type".parse().unwrap(),
            );
            headers.insert("access-control-max-age", "3600".parse().unwrap());
            resp
        })
    })
}

/// Wrap `inner` so every request/response pair is logged via `tracing`.
/// Poem-free equivalent of `poem::middleware::Tracing`.
pub fn with_tracing(inner: Handler) -> Handler {
    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        Box::pin(async move {
            let resp = inner(req, params).await;
            tracing::info!(%method, %path, status = %resp.status(), "request");
            resp
        })
    })
}

/// Wrap `inner` with a per-client rate limit backed by `limiter`. Shared
/// across every route in an app so the budget is global, not per-route —
/// build one `Arc<RateLimiter>` per app with [`build_rate_limiter`] and
/// pass clones of it to each route's wrapper.
pub fn with_shared_rate_limit(inner: Handler, limiter: Arc<RateLimiter>) -> Handler {
    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let limiter = Arc::clone(&limiter);
        let key = req
            .headers()
            .get("x-forwarded-for")
            .or_else(|| req.headers().get("x-real-ip"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_else(|| "anonymous".to_string());

        Box::pin(async move {
            if limiter.check(&key, chrono::Utc::now()).is_err() {
                return empty_status(StatusCode::TOO_MANY_REQUESTS);
            }
            inner(req, params).await
        })
    })
}

/// Construct a rate limiter suitable for [`with_shared_rate_limit`].
pub fn build_rate_limiter(max_requests: u32, window_secs: i64) -> Arc<RateLimiter> {
    Arc::new(RateLimiter::new(max_requests, chrono::Duration::seconds(window_secs)))
}

/// Wrap `inner` with a per-client rate limit, keyed the same way as the
/// poem `RateLimit` middleware (`X-Forwarded-For` / `X-Real-IP`, falling
/// back to a single shared bucket). Convenience wrapper over
/// [`with_shared_rate_limit`] for callers that only need a single route
/// (e.g. tests); apps with multiple routes should build one limiter with
/// [`build_rate_limiter`] and share it via [`with_shared_rate_limit`].
pub fn with_rate_limit(inner: Handler, max_requests: u32, window_secs: i64) -> Handler {
    with_shared_rate_limit(inner, build_rate_limiter(max_requests, window_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyper_compat::{serve, Router};
    use hyper::Method;

    fn ok_handler() -> Handler {
        Arc::new(|_req, _params| Box::pin(async { empty_status(StatusCode::OK) }))
    }

    #[tokio::test]
    async fn cors_adds_headers_and_answers_preflight() {
        let router = Router::new()
            .route(Method::GET, "/x", with_cors(ok_handler()))
            .route(Method::OPTIONS, "/x", with_cors(ok_handler()));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .request(reqwest::Method::OPTIONS, format!("http://{addr}/x"))
            .header("origin", "https://example.com")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(resp.headers().contains_key("access-control-allow-origin"));
    }

    #[tokio::test]
    async fn rate_limit_blocks_after_threshold() {
        let router = Router::new().route(Method::GET, "/x", with_rate_limit(ok_handler(), 2, 60));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        assert_eq!(
            client.get(format!("http://{addr}/x")).send().await.unwrap().status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            client.get(format!("http://{addr}/x")).send().await.unwrap().status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            client.get(format!("http://{addr}/x")).send().await.unwrap().status(),
            reqwest::StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn rate_limit_separate_keys_get_separate_budgets() {
        let router = Router::new().route(Method::GET, "/x", with_rate_limit(ok_handler(), 1, 60));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(format!("http://{addr}/x"))
                .header("x-forwarded-for", "1.1.1.1")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            client
                .get(format!("http://{addr}/x"))
                .header("x-forwarded-for", "2.2.2.2")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );
        assert_eq!(
            client
                .get(format!("http://{addr}/x"))
                .header("x-forwarded-for", "1.1.1.1")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::TOO_MANY_REQUESTS
        );
    }
}

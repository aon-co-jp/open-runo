//! Poem-free middleware for the hyper_compat stack: plain functions that
//! wrap a [`Handler`] and return a new [`Handler`] ("function in, function
//! out" composition — see `hyper_compat.rs` module doc), rather than
//! reimplementing `poem::Middleware`/`Endpoint` traits.

use crate::hyper_compat::{empty_status, json_response, Handler, Response};
use hyper::StatusCode;
use open_runo_api_types::RateLimitedResponse;
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

/// Wrap `inner` so every request/response pair is logged via `tracing`,
/// tagged with a request ID that also comes back as the `X-Request-Id`
/// response header. If the caller already sent an `X-Request-Id` (e.g. a
/// load balancer, or a client chaining its own trace), that value is
/// reused instead of minting a new one, so a single request can be
/// correlated end-to-end across hops. Otherwise a fresh UUID v4 is
/// generated. Clients (the WASM frontend, `open-runo-cli`) surface this
/// ID in error messages so a user can hand it to whoever reads the
/// server's logs instead of describing "it just failed".
pub fn with_tracing(inner: Handler) -> Handler {
    Arc::new(move |req, params| {
        let inner = Arc::clone(&inner);
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let request_id = req
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Box::pin(async move {
            let mut resp = inner(req, params).await;
            tracing::info!(%method, %path, status = %resp.status(), %request_id, "request");
            if let Ok(value) = request_id.parse() {
                resp.headers_mut().insert("x-request-id", value);
            }
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
            let now = chrono::Utc::now();
            if limiter.check(&key, now).is_err() {
                let retry_after_secs = limiter.seconds_until_reset(&key, now);
                let mut resp = json_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    &RateLimitedResponse {
                        error: "rate limit exceeded, see retry_after_secs".to_string(),
                        retry_after_secs,
                    },
                );
                if let Ok(value) = retry_after_secs.to_string().parse() {
                    resp.headers_mut().insert("retry-after", value);
                }
                return resp;
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
    async fn rate_limit_response_has_retry_after_header_and_typed_body() {
        let router = Router::new().route(Method::GET, "/x", with_rate_limit(ok_handler(), 1, 60));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let _ = client.get(format!("http://{addr}/x")).send().await.unwrap();
        let blocked = client.get(format!("http://{addr}/x")).send().await.unwrap();
        assert_eq!(blocked.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        let retry_after_header: i64 = blocked
            .headers()
            .get("retry-after")
            .expect("retry-after header should be present")
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=60).contains(&retry_after_header));

        let body: open_runo_api_types::RateLimitedResponse = blocked.json().await.unwrap();
        assert_eq!(body.retry_after_secs, retry_after_header);
    }

    #[tokio::test]
    async fn tracing_assigns_a_request_id_when_caller_sends_none() {
        let router = Router::new().route(Method::GET, "/x", with_tracing(ok_handler()));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new().get(format!("http://{addr}/x")).send().await.unwrap();
        let request_id = resp
            .headers()
            .get("x-request-id")
            .expect("x-request-id header should be present")
            .to_str()
            .unwrap();
        assert!(uuid::Uuid::parse_str(request_id).is_ok(), "should be a valid UUID: {request_id}");
    }

    #[tokio::test]
    async fn tracing_echoes_a_caller_supplied_request_id() {
        let router = Router::new().route(Method::GET, "/x", with_tracing(ok_handler()));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/x"))
            .header("x-request-id", "caller-chosen-id-123")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.headers().get("x-request-id").unwrap().to_str().unwrap(), "caller-chosen-id-123");
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

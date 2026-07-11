//! Poem-free HTTP foundation: hand-rolled router + response helpers on top
//! of `hyper` directly. New handlers are migrated onto this module one at a
//! time; once every handler and middleware has moved here, `poem` is
//! dropped from this crate's `Cargo.toml` entirely.
//!
//! Design (see CLAUDE.md HANDOFF for the full migration plan):
//! - Handlers are plain `async fn(Request) -> Response` closures capturing
//!   `Arc<AppState>` etc. — no `#[handler]` macro, no `Endpoint` trait.
//! - Middleware is "function in, function out" composition, not a trait
//!   hierarchy.
//! - Routing is a small path+method table with manual `:param` matching
//!   (no external router crate needed yet at this scale).

use bytes::Bytes;
use futures::Stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Method, Request as HyperRequest, Response as HyperResponse, StatusCode};
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Boxed so both fixed bodies (`json_response`) and streamed ones
/// (`sse_response`) can share a single `Response` type.
pub type Body = BoxBody<Bytes, Infallible>;
pub type Request = HyperRequest<Incoming>;
pub type Response = HyperResponse<Body>;
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type Handler = Arc<dyn Fn(Request, Params) -> BoxFuture<Response> + Send + Sync>;

/// Path parameters extracted from a matched route, e.g. `:table` → value.
#[derive(Debug, Default, Clone)]
pub struct Params(pub HashMap<String, String>);

impl Params {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }
}

/// Wrap raw bytes as a fixed (non-streaming) [`Body`]. `pub` so other
/// modules (e.g. `middleware_hyper::with_compression`, which needs to
/// substitute a gzip-compressed byte buffer for an existing response body)
/// can build a `Body` without duplicating the `Full`/`boxed()` plumbing.
pub fn fixed_body(bytes: Bytes) -> Body {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}

/// Build a JSON response with the given status code.
pub fn json_response(status: StatusCode, value: &impl serde::Serialize) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    HyperResponse::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(fixed_body(Bytes::from(body)))
        .expect("building a response from a fixed set of valid headers cannot fail")
}

pub fn empty_status(status: StatusCode) -> Response {
    HyperResponse::builder()
        .status(status)
        .body(fixed_body(Bytes::new()))
        .expect("building a response from a fixed set of valid headers cannot fail")
}

/// Build an `text/html; charset=utf-8` response (e.g. for GraphiQL).
pub fn html_response(status: StatusCode, html: impl Into<String>) -> Response {
    HyperResponse::builder()
        .status(status)
        .header("content-type", "text/html; charset=utf-8")
        .body(fixed_body(Bytes::from(html.into())))
        .expect("building a response from a fixed set of valid headers cannot fail")
}

/// One Server-Sent Event: an optional `event:` type and its `data:` payload.
pub struct SseEvent {
    pub event_type: Option<&'static str>,
    pub data: String,
}

impl SseEvent {
    fn encode(&self) -> Bytes {
        let mut out = String::new();
        if let Some(ty) = self.event_type {
            out.push_str("event: ");
            out.push_str(ty);
            out.push('\n');
        }
        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        Bytes::from(out)
    }
}

/// Build a `text/event-stream` response from a stream of [`SseEvent`]s.
/// Poem-free equivalent of `poem::web::sse::SSE`.
pub fn sse_response<S>(stream: S) -> Response
where
    S: Stream<Item = SseEvent> + Send + Sync + 'static,
{
    use futures::StreamExt;
    let frame_stream = stream.map(|event| Ok::<_, Infallible>(Frame::data(event.encode())));
    let body: Body = BodyExt::boxed(StreamBody::new(frame_stream));
    HyperResponse::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(body)
        .expect("building a response from a fixed set of valid headers cannot fail")
}

/// Parse the request's `?a=1&b=2` query string into a lookup map. Minimal
/// percent-decoding (`%XX`, `+` → space) — no external query-string crate
/// needed at this scale.
pub fn query_params(req: &Request) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let Some(query) = req.uri().query() else {
        return params;
    };
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(key), percent_decode(value));
    }
    params
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub async fn read_json_body<T: serde::de::DeserializeOwned>(
    req: Request,
) -> Result<T, Response> {
    let bytes = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({ "error": "failed to read request body" }),
            ))
        }
    };
    serde_json::from_slice::<T>(&bytes).map_err(|e| {
        json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": format!("invalid JSON body: {e}") }),
        )
    })
}

/// `GET /health` and `GET /healthz` — poem-free equivalent of the handler
/// in `lib.rs`. Kept in lockstep with that JSON shape until the poem
/// version is retired.
pub fn health_handler() -> Handler {
    #[derive(serde::Serialize)]
    struct Health {
        status: &'static str,
        service: &'static str,
        version: &'static str,
    }

    Arc::new(move |_req, _params| {
        Box::pin(async move {
            json_response(
                StatusCode::OK,
                &Health {
                    status: "ok",
                    service: "open-runo-router",
                    version: env!("CARGO_PKG_VERSION"),
                },
            )
        })
    })
}

/// Serve a single static file from disk at request time, with a fixed
/// `content-type`. Used to host the WASM frontend bundle (`www/index.html`,
/// `www/pkg/*.js`, `www/pkg/*.wasm`) directly from `open-runo-router` —
/// no separate static-file server or Node.js tooling required.
pub fn static_file_handler(path: std::path::PathBuf, content_type: &'static str) -> Handler {
    Arc::new(move |_req, _params| {
        let path = path.clone();
        Box::pin(async move {
            match tokio::fs::read(&path).await {
                Ok(bytes) => HyperResponse::builder()
                    .status(StatusCode::OK)
                    .header("content-type", content_type)
                    .body(fixed_body(Bytes::from(bytes)))
                    .expect("building a response from a fixed set of valid headers cannot fail"),
                Err(_) => empty_status(StatusCode::NOT_FOUND),
            }
        })
    })
}

/// Serve `router` over a real TCP listener; returns the bound address and a
/// task handle. Used by tests (and, eventually, `main.rs`) to run the
/// poem-free stack end to end.
pub async fn serve(router: Router, addr: std::net::SocketAddr) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    use hyper::server::conn::http1;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;
    let router = Arc::new(router);

    let handle = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let io = TokioIo::new(stream);
            let router = Arc::clone(&router);
            let service = hyper::service::service_fn(move |req: Request| {
                let router = Arc::clone(&router);
                async move { Ok::<_, std::convert::Infallible>(router.dispatch(req).await) }
            });
            tokio::spawn(async move {
                let _ = http1::Builder::new().serve_connection(io, service).await;
            });
        }
    });

    Ok((bound_addr, handle))
}

/// A single registered route: method + path pattern (`:name` segments) + handler.
struct Route {
    method: Method,
    segments: Vec<Segment>,
    handler: Handler,
}

#[derive(Clone, PartialEq)]
enum Segment {
    Literal(String),
    Param(String),
}

fn parse_pattern(pattern: &str) -> Vec<Segment> {
    pattern
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(name) = s.strip_prefix(':') {
                Segment::Param(name.to_string())
            } else {
                Segment::Literal(s.to_string())
            }
        })
        .collect()
}

/// Minimal method+path router. Not a general-purpose crate replacement —
/// just enough to dispatch open-runo-router's fixed endpoint set.
#[derive(Default)]
pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn route(mut self, method: Method, pattern: &str, handler: Handler) -> Self {
        self.routes.push(Route {
            method,
            segments: parse_pattern(pattern),
            handler,
        });
        self
    }

    /// Register an `OPTIONS` route, wrapped in
    /// [`crate::middleware_hyper::with_cors`], for every distinct path
    /// pattern that has at least one other method registered but no
    /// `OPTIONS` handler of its own yet. Call this once, after all real
    /// routes are registered.
    ///
    /// Without this, [`Self::dispatch`] answers `OPTIONS` on any such
    /// path with a bare `405` -- generated by the router itself, before
    /// any handler (including CORS middleware) ever runs -- so a
    /// browser's CORS preflight (required whenever a cross-origin
    /// request sends a non-simple header like `X-Api-Key`, or most
    /// non-GET methods) can never succeed. This was a real, previously
    /// undetected bug: every route in production wired only its actual
    /// method (`GET`/`POST`/etc.), never `OPTIONS`, so cross-origin
    /// browser calls to almost the entire API were silently broken.
    /// Found via an actual cross-origin browser test, not just unit
    /// tests wrapping a single synthetic route in `with_cors` at both
    /// `GET` and `OPTIONS` (see CLAUDE.md HANDOFF, 2026-07-11).
    pub fn with_cors_preflight(mut self) -> Self {
        let candidates: Vec<Vec<Segment>> = self
            .routes
            .iter()
            .filter(|r| r.method != Method::OPTIONS)
            .map(|r| r.segments.clone())
            .collect();

        let mut added: Vec<Vec<Segment>> = Vec::new();
        for segments in candidates {
            let already_registered = self
                .routes
                .iter()
                .any(|r| r.method == Method::OPTIONS && r.segments == segments);
            let already_queued = added.iter().any(|s| *s == segments);
            if already_registered || already_queued {
                continue;
            }
            added.push(segments.clone());
            self.routes.push(Route {
                method: Method::OPTIONS,
                segments,
                // `with_cors` answers OPTIONS itself (see its `is_preflight`
                // branch) without ever calling the inner handler, so this
                // inner handler is never actually invoked.
                handler: crate::middleware_hyper::with_cors(Arc::new(|_req, _params| {
                    Box::pin(async { empty_status(StatusCode::OK) })
                })),
            });
        }
        self
    }

    fn match_path(&self, route: &Route, path: &str) -> Option<Params> {
        let path_segments: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
        if path_segments.len() != route.segments.len() {
            return None;
        }
        let mut params = HashMap::new();
        for (seg, actual) in route.segments.iter().zip(path_segments.iter()) {
            match seg {
                Segment::Literal(lit) => {
                    if lit != actual {
                        return None;
                    }
                }
                Segment::Param(name) => {
                    params.insert(name.clone(), actual.to_string());
                }
            }
        }
        Some(Params(params))
    }

    pub fn dispatch(&self, req: Request) -> BoxFuture<Response> {
        let path = req.uri().path().to_string();
        let method = req.method().clone();

        for route in &self.routes {
            if route.method != method {
                continue;
            }
            if let Some(params) = self.match_path(route, &path) {
                let handler = Arc::clone(&route.handler);
                return handler(req, params);
            }
        }

        // Path matched by a different method → 405; otherwise 404.
        let path_exists = self
            .routes
            .iter()
            .any(|r| self.match_path(r, &path).is_some());
        let status = if path_exists {
            StatusCode::METHOD_NOT_ALLOWED
        } else {
            StatusCode::NOT_FOUND
        };
        Box::pin(async move { empty_status(status) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn h(status: StatusCode) -> Handler {
        Arc::new(move |_req, _params| Box::pin(async move { empty_status(status) }))
    }

    #[test]
    fn percent_decode_handles_plus_and_hex_escapes() {
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("a%2Fb%20c"), "a/b c");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn parses_literal_and_param_segments() {
        let segs = parse_pattern("/api/db/:table/:key");
        assert_eq!(segs.len(), 4);
        assert!(matches!(&segs[0], Segment::Literal(s) if s == "api"));
        assert!(matches!(&segs[1], Segment::Literal(s) if s == "db"));
        assert!(matches!(&segs[2], Segment::Param(s) if s == "table"));
        assert!(matches!(&segs[3], Segment::Param(s) if s == "key"));
    }

    #[tokio::test]
    async fn json_response_has_expected_content_type() {
        let resp = json_response(StatusCode::OK, &json!({ "ok": true }));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn router_matches_literal_then_param_routes() {
        let router = Router::new()
            .route(Method::GET, "/health", h(StatusCode::OK))
            .route(Method::GET, "/api/db/:table/:key", h(StatusCode::IM_A_TEAPOT));

        let health = router.routes.iter().find(|r| r.method == Method::GET && matches!(r.segments.first(), Some(Segment::Literal(s)) if s == "health"));
        assert!(health.is_some());

        let dyn_route = &router.routes[1];
        let params = router.match_path(dyn_route, "/api/db/users/42").unwrap();
        assert_eq!(params.get("table"), Some("users"));
        assert_eq!(params.get("key"), Some("42"));
    }

    #[test]
    fn router_rejects_mismatched_segment_count() {
        let router = Router::new().route(Method::GET, "/api/db/:table/:key", h(StatusCode::OK));
        let route = &router.routes[0];
        assert!(router.match_path(route, "/api/db/users").is_none());
        assert!(router.match_path(route, "/api/db/users/42/extra").is_none());
    }

    #[tokio::test]
    async fn cors_preflight_reaches_a_real_handler_instead_of_a_bare_405() {
        // Before `with_cors_preflight`, an OPTIONS request to a path that
        // only registered GET/PUT/DELETE fell through Router::dispatch's
        // own 405 fallback -- generated by the router itself, before any
        // handler (including with_cors) ever ran. This reproduces exactly
        // that shape (multiple methods, no explicit OPTIONS route) and
        // confirms the auto-added preflight route now answers instead.
        let router = Router::new()
            .route(Method::GET, "/api/db/:table/:key", h(StatusCode::OK))
            .route(Method::PUT, "/api/db/:table/:key", h(StatusCode::OK))
            .route(Method::DELETE, "/api/db/:table/:key", h(StatusCode::OK))
            .with_cors_preflight();

        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let resp = reqwest::Client::new()
            .request(reqwest::Method::OPTIONS, format!("http://{addr}/api/db/users/42"))
            .header("origin", "https://example.com")
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(resp.headers().contains_key("access-control-allow-origin"));
    }

    #[test]
    fn with_cors_preflight_does_not_duplicate_an_explicit_options_route() {
        let router = Router::new()
            .route(Method::GET, "/x", h(StatusCode::OK))
            .route(Method::OPTIONS, "/x", h(StatusCode::IM_A_TEAPOT))
            .with_cors_preflight();

        let options_routes: Vec<_> = router.routes.iter().filter(|r| r.method == Method::OPTIONS).collect();
        assert_eq!(options_routes.len(), 1, "should not add a second OPTIONS route for the same path");
    }

    #[test]
    fn with_cors_preflight_adds_exactly_one_options_route_per_distinct_path() {
        // /api/db/:table/:key is registered under 3 methods; it should
        // still only get ONE auto-added OPTIONS route, not three.
        let router = Router::new()
            .route(Method::GET, "/api/db/:table/:key", h(StatusCode::OK))
            .route(Method::PUT, "/api/db/:table/:key", h(StatusCode::OK))
            .route(Method::DELETE, "/api/db/:table/:key", h(StatusCode::OK))
            .with_cors_preflight();

        let options_routes: Vec<_> = router.routes.iter().filter(|r| r.method == Method::OPTIONS).collect();
        assert_eq!(options_routes.len(), 1);
    }

    /// End-to-end: real TCP listener, real hyper connection, real HTTP
    /// client — proves the poem-free stack actually serves traffic, not
    /// just that in-process function calls type-check.
    #[tokio::test]
    async fn health_endpoint_serves_over_real_http() {
        let router = Router::new()
            .route(Method::GET, "/health", health_handler())
            .route(Method::GET, "/healthz", health_handler());

        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");

        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("valid json body");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "open-runo-router");

        let resp = client
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = client
            .get(format!("http://{addr}/nonexistent"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn static_file_handler_serves_existing_file_and_404s_missing() {
        let dir = std::env::temp_dir().join(format!("orn-static-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("hello.txt");
        std::fs::write(&file_path, b"hello static world").unwrap();

        let router = Router::new()
            .route(Method::GET, "/hello.txt", static_file_handler(file_path.clone(), "text/plain"))
            .route(Method::GET, "/missing.txt", static_file_handler(dir.join("missing.txt"), "text/plain"));
        let (addr, _handle) = serve(router, "127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind ephemeral port");
        let client = reqwest::Client::new();

        let resp = client
            .get(format!("http://{addr}/hello.txt"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "text/plain");
        assert_eq!(resp.text().await.unwrap(), "hello static world");

        let resp = client
            .get(format!("http://{addr}/missing.txt"))
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! gRPC — Poem-parity gap ("gRPC(poem-grpc相当)", `docs/poem-parity.md`).
//!
//! A minimal, real gRPC-over-HTTP/2 server: not a general-purpose gRPC
//! framework (no reflection, no streaming, no code generation from
//! `.proto` files), but a working implementation of one real, well-known
//! service -- [`grpc.health.v1.Health`](https://github.com/grpc/grpc/blob/master/doc/health-checking.md)'s
//! `Check` unary RPC -- proving the transport (HTTP/2 framing, the gRPC
//! length-prefixed message envelope, trailers-based `grpc-status`) and the
//! wire format (Protocol Buffers) both work end to end.
//!
//! **No new dependencies.** HTTP/2 comes from `hyper`'s existing `full`
//! feature (already pulls in the `h2` crate transitively; this module is
//! the first thing in the crate to actually construct an
//! `hyper::server::conn::http2::Builder`, but the dependency itself was
//! already present). Protobuf encoding/decoding is hand-rolled for the
//! two tiny messages this module actually needs
//! (`HealthCheckRequest`/`HealthCheckResponse`) rather than pulling in a
//! full protobuf codegen pipeline (`prost`/`tonic`) -- the wire format for
//! these two messages is a handful of varints and one length-delimited
//! string, well within the "hand-roll the data shape" precedent this
//! crate already sets for multipart and WebSocket framing.
//!
//! **Why a separate port, not the same listener as the REST API**: HTTP/2
//! without TLS ("h2c") normally requires either an `Upgrade: h2c` header
//! on an HTTP/1.1 request or "prior knowledge" (the client sends the
//! HTTP/2 connection preface immediately). Distinguishing an incoming
//! plain-TCP connection's protocol before the first bytes arrive would
//! require protocol sniffing on every connection to the main REST
//! listener, adding complexity and latency there for a feature most
//! deployments won't use. A dedicated port (the same pattern
//! `hyper_compat::tls::serve_tls` uses for TLS) keeps the common path
//! simple; gRPC-aware clients connect straight to it with prior knowledge.

use bytes::{Bytes, BytesMut};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{HeaderMap, Request as HyperRequest, Response as HyperResponse, StatusCode};
use std::convert::Infallible;

type Request = HyperRequest<Incoming>;
type Response = HyperResponse<BoxBody<Bytes, Infallible>>;

/// gRPC status codes actually used here (a small subset of the full
/// [status code table](https://grpc.io/docs/guides/status-codes/)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrpcStatus {
    Ok = 0,
    Unknown = 2,
    InvalidArgument = 3,
    Unimplemented = 12,
    Internal = 13,
}

// ── gRPC message framing (not protobuf itself -- the length-prefix
// envelope every gRPC message body is wrapped in, per
// https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md) ────────

/// Strip the 5-byte gRPC frame header (1 compression-flag byte + 4
/// big-endian length bytes) off `body` and return the inner message
/// bytes. Rejects a compressed frame (flag byte != 0) -- this
/// implementation never sends `grpc-encoding`, so a compressed request
/// would mean a client assuming compression was negotiated when it
/// wasn't.
fn decode_grpc_frame(body: &[u8]) -> Result<&[u8], GrpcStatus> {
    if body.len() < 5 {
        return Err(GrpcStatus::InvalidArgument);
    }
    let compressed = body[0] != 0;
    if compressed {
        return Err(GrpcStatus::Unimplemented);
    }
    let len = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    let message = body.get(5..5 + len).ok_or(GrpcStatus::InvalidArgument)?;
    Ok(message)
}

/// Wrap `message` in a gRPC frame (uncompressed).
fn encode_grpc_frame(message: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(5 + message.len());
    out.extend_from_slice(&[0u8]); // uncompressed
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out.freeze()
}

// ── Minimal hand-rolled Protocol Buffers codec ───────────────────────────
// Just enough of the wire format (varints, length-delimited fields) for
// the two messages this module needs. Not a general-purpose protobuf
// implementation.

fn encode_varint(mut value: u64, out: &mut BytesMut) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.extend_from_slice(&[byte]);
            break;
        }
        out.extend_from_slice(&[byte | 0x80]);
    }
}

fn decode_varint(buf: &[u8]) -> Option<(u64, &[u8])> {
    let mut value: u64 = 0;
    for (i, &byte) in buf.iter().enumerate().take(10) {
        value |= ((byte & 0x7f) as u64) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, &buf[i + 1..]));
        }
    }
    None
}

/// `grpc.health.v1.HealthCheckRequest { string service = 1; }` -- the
/// `service` field is optional in real health-check clients (empty string
/// means "overall server health"); this decoder tolerates a completely
/// empty message the same way.
struct HealthCheckRequest {
    #[allow(dead_code)] // parsed for completeness/interop; not branched on
    service: String,
}

fn decode_health_check_request(bytes: &[u8]) -> Result<HealthCheckRequest, GrpcStatus> {
    let mut service = String::new();
    let mut buf = bytes;
    while !buf.is_empty() {
        let Some((tag, rest)) = decode_varint(buf) else {
            return Err(GrpcStatus::InvalidArgument);
        };
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        buf = rest;
        match (field_number, wire_type) {
            (1, 2) => {
                let Some((len, rest)) = decode_varint(buf) else {
                    return Err(GrpcStatus::InvalidArgument);
                };
                let len = len as usize;
                let Some(value_bytes) = rest.get(..len) else {
                    return Err(GrpcStatus::InvalidArgument);
                };
                service = String::from_utf8_lossy(value_bytes).into_owned();
                buf = &rest[len..];
            }
            // Unknown field: skip it minimally-correctly for the two wire
            // types this tiny decoder might plausibly see; anything else
            // is rejected rather than silently mis-parsed.
            (_, 0) => {
                let Some((_, rest)) = decode_varint(buf) else {
                    return Err(GrpcStatus::InvalidArgument);
                };
                buf = rest;
            }
            (_, 2) => {
                let Some((len, rest)) = decode_varint(buf) else {
                    return Err(GrpcStatus::InvalidArgument);
                };
                let len = len as usize;
                if rest.len() < len {
                    return Err(GrpcStatus::InvalidArgument);
                }
                buf = &rest[len..];
            }
            _ => return Err(GrpcStatus::InvalidArgument),
        }
    }
    Ok(HealthCheckRequest { service })
}

/// `grpc.health.v1.HealthCheckResponse.ServingStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingStatus {
    Unknown = 0,
    Serving = 1,
    NotServing = 2,
}

/// `grpc.health.v1.HealthCheckResponse { ServingStatus status = 1; }`.
fn encode_health_check_response(status: ServingStatus) -> Bytes {
    let mut out = BytesMut::new();
    encode_varint((1 << 3) | 0, &mut out); // field 1, wire type 0 (varint)
    encode_varint(status as u64, &mut out);
    out.freeze()
}

// ── HTTP/2 <-> gRPC glue ──────────────────────────────────────────────────

/// Build a gRPC-framed, trailers-terminated unary response: one DATA frame
/// (the encoded message) followed by a TRAILERS frame carrying
/// `grpc-status` (and `grpc-message` on failure). Per
/// [the gRPC-over-HTTP2 spec](https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md#responses),
/// `grpc-status` is always a trailer, never a leading header, even for a
/// successful unary call.
fn grpc_response(status: GrpcStatus, message: Option<&[u8]>) -> Response {
    let mut frames: Vec<Result<Frame<Bytes>, Infallible>> = Vec::with_capacity(2);
    if let Some(message) = message {
        frames.push(Ok(Frame::data(encode_grpc_frame(message))));
    }
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", (status as i32).to_string().parse().unwrap());
    trailers.insert("grpc-message", "".parse().unwrap());
    frames.push(Ok(Frame::trailers(trailers)));

    let body: BoxBody<Bytes, Infallible> = BodyExt::boxed(StreamBody::new(futures::stream::iter(frames)));
    HyperResponse::builder()
        .status(StatusCode::OK) // gRPC status is orthogonal to the HTTP status -- 200 even on grpc-status != 0
        .header("content-type", "application/grpc+proto")
        .body(body)
        .expect("building a response from a fixed set of valid headers cannot fail")
}

/// `POST /grpc.health.v1.Health/Check` -- the one real RPC this module
/// implements. Always reports `SERVING` (this server has no notion of
/// per-dependency health beyond "the process is up and answering");
/// wiring in real subsystem checks (DB connectivity, etc.) is a natural
/// follow-up once more of `AppState` needs to be reachable from here.
async fn health_check_handler(req: Request) -> Response {
    let collected = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return grpc_response(GrpcStatus::Internal, None),
    };
    let message = match decode_grpc_frame(&collected) {
        Ok(m) => m,
        Err(status) => return grpc_response(status, None),
    };
    if decode_health_check_request(message).is_err() {
        return grpc_response(GrpcStatus::InvalidArgument, None);
    }
    let response_bytes = encode_health_check_response(ServingStatus::Serving);
    grpc_response(GrpcStatus::Ok, Some(&response_bytes))
}

/// Serve the gRPC health-check service over real HTTP/2 (h2c, prior
/// knowledge -- no TLS required, matching how most internal/dev gRPC
/// traffic runs). Returns the bound address and a task handle, same
/// shape as `hyper_compat::serve`/`hyper_compat::tls::serve_tls`.
pub async fn serve_grpc(addr: std::net::SocketAddr) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req: Request| async move {
                    let path = req.uri().path();
                    let resp = if path == "/grpc.health.v1.Health/Check" {
                        health_check_handler(req).await
                    } else {
                        grpc_response(GrpcStatus::Unimplemented, None)
                    };
                    Ok::<Response, Infallible>(resp)
                });
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await;
            });
        }
    });

    Ok((bound_addr, handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips_small_and_large_values() {
        for value in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64] {
            let mut buf = BytesMut::new();
            encode_varint(value, &mut buf);
            let (decoded, rest) = decode_varint(&buf).unwrap();
            assert_eq!(decoded, value);
            assert!(rest.is_empty());
        }
    }

    #[test]
    fn grpc_frame_round_trips() {
        let message = b"hello grpc";
        let framed = encode_grpc_frame(message);
        let decoded = decode_grpc_frame(&framed).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn decode_grpc_frame_rejects_truncated_input() {
        assert_eq!(decode_grpc_frame(&[0, 0, 0]), Err(GrpcStatus::InvalidArgument));
    }

    #[test]
    fn decode_grpc_frame_rejects_compressed_flag() {
        let mut framed = encode_grpc_frame(b"x").to_vec();
        framed[0] = 1; // claim compression, which this server never negotiates
        assert_eq!(decode_grpc_frame(&framed), Err(GrpcStatus::Unimplemented));
    }

    #[test]
    fn health_check_response_encodes_serving_as_field_1_value_1() {
        let bytes = encode_health_check_response(ServingStatus::Serving);
        // field 1 (status), wire type 0 (varint) -> tag byte 0x08, then
        // varint(1) -> single byte 0x01. This is exactly what any real
        // protobuf decoder (protoc, prost, etc.) would produce/expect for
        // `HealthCheckResponse { status: SERVING }`.
        assert_eq!(bytes.as_ref(), &[0x08, 0x01]);
    }

    #[test]
    fn decode_health_check_request_accepts_empty_message() {
        // A HealthCheckRequest with no `service` field set serializes to
        // zero bytes in protobuf -- decoding that must succeed with the
        // default (empty) value, not error.
        let req = decode_health_check_request(&[]).unwrap();
        assert_eq!(req.service, "");
    }

    #[test]
    fn decode_health_check_request_parses_service_field() {
        // Hand-encode `HealthCheckRequest { service: "orders" }`: tag
        // (field 1, wire type 2) = 0x0a, length = 6, then the UTF-8 bytes.
        let mut bytes = vec![0x0a, 6];
        bytes.extend_from_slice(b"orders");
        let req = decode_health_check_request(&bytes).unwrap();
        assert_eq!(req.service, "orders");
    }

    /// End-to-end: a real HTTP/2 server (h2c, prior knowledge) answering a
    /// real gRPC unary call, verified with hyper's own HTTP/2 client --
    /// not just a same-process function call. Proves the framing,
    /// trailers-based grpc-status, and protobuf encode/decode all agree
    /// with each other over an actual TCP connection.
    #[tokio::test]
    async fn health_check_serves_over_real_http2() {
        use http_body_util::{BodyExt, Full};
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_util::client::legacy::Client;
        use hyper_util::rt::TokioExecutor;

        let (addr, _handle) = serve_grpc("127.0.0.1:0".parse().unwrap()).await.expect("bind grpc port");

        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        let client: Client<_, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).http2_only(true).build(connector);

        // Empty HealthCheckRequest (no `service` field set).
        let request_body = encode_grpc_frame(&[]);
        let req = HyperRequest::builder()
            .method("POST")
            .uri(format!("http://{addr}/grpc.health.v1.Health/Check"))
            .header("content-type", "application/grpc+proto")
            .body(Full::new(request_body))
            .unwrap();

        let resp = client.request(req).await.expect("HTTP/2 request should succeed");
        assert_eq!(resp.status(), StatusCode::OK);

        let collected = resp.collect().await.expect("collecting body+trailers should succeed");
        let trailers = collected.trailers().cloned().unwrap_or_default();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("0"),
            "grpc-status should be OK (0), carried as an HTTP/2 trailer"
        );

        let body_bytes = collected.to_bytes();
        let message = decode_grpc_frame(&body_bytes).expect("response should be a valid gRPC frame");
        // HealthCheckResponse { status: SERVING } -> [0x08, 0x01], per the
        // dedicated unit test above.
        assert_eq!(message, &[0x08, 0x01]);

        // Also exercise the "unknown method" path over the same real
        // connection, to prove routing (not just the one happy-path RPC)
        // works end to end.
        let req2 = HyperRequest::builder()
            .method("POST")
            .uri(format!("http://{addr}/no.such.Service/Method"))
            .body(Full::new(Bytes::new()))
            .unwrap();
        let resp2 = client.request(req2).await.expect("HTTP/2 request should succeed");
        let trailers2 = resp2.collect().await.unwrap().trailers().cloned().unwrap_or_default();
        assert_eq!(trailers2.get("grpc-status").and_then(|v| v.to_str().ok()), Some("12")); // UNIMPLEMENTED
    }
}

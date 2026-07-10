//! REST API client for open-runo-router, callable from WASM.
//!
//! Poem-free/Tauri-free equivalent of the old `src/api/client.ts`
//! `invoke()`-style helpers: plain async Rust functions that `fetch()` the
//! backend directly, decoding JSON via `serde`.

use serde::Deserialize;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

#[derive(Debug, Deserialize)]
pub struct Health {
    pub status: String,
    pub service: String,
    pub version: String,
}

/// Base URL for API calls. Empty string means same-origin (the WASM
/// bundle is served by the same open-runo-router binary it talks to).
fn base_url() -> &'static str {
    ""
}

async fn get_json<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T, String> {
    let opts = RequestInit::new();
    opts.set_method("GET");
    opts.set_mode(RequestMode::SameOrigin);

    let url = format!("{}{path}", base_url());
    let request = Request::new_with_str_and_init(&url, &opts).map_err(|e| format!("{e:?}"))?;

    let window = web_sys::window().ok_or("no window")?;
    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("fetch error: {e:?}"))?;
    let resp: Response = resp_value.dyn_into().map_err(|e| format!("{e:?}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let json = JsFuture::from(resp.json().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("body read error: {e:?}"))?;

    serde_wasm_bindgen::from_value(json).map_err(|e| format!("decode error: {e}"))
}

pub async fn health_check() -> Result<Health, String> {
    get_json::<Health>("/health").await
}

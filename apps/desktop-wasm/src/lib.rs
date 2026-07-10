//! open-runo desktop frontend, compiled to WebAssembly.
//!
//! Replaces the former Tauri + TypeScript + Bootstrap + Node.js stack.
//! Rust is the only language for both frontend and backend; this crate
//! compiles to `wasm32-unknown-unknown` and runs directly in a webview or
//! browser via `wasm-bindgen`'s generated JS glue (a thin loader, not a
//! build toolchain — no webpack/vite/tsc in this crate's own pipeline).
//!
//! `invoke()`-style calls (the one thing Tauri provided that's worth
//! keeping compatibility with) are implemented here as plain async
//! functions calling the REST API directly via `web_sys::window().fetch()`,
//! rather than an IPC bridge to a separate host process.

use wasm_bindgen::prelude::*;

mod api;

#[wasm_bindgen(start)]
pub fn start() {
    console_log("open-runo-desktop-wasm starting");
    mount_shell();
}

fn console_log(msg: &str) {
    web_sys::console::log_1(&JsValue::from_str(msg));
}

/// Find the page's `#content` element (owned by the host HTML — see
/// `www/index.html`) and kick off the initial health-check render.
/// Equivalent to the old `main.ts`'s startup, but written in Rust and
/// driven by `web_sys` DOM calls instead of a TypeScript bundler output.
fn mount_shell() {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        console_log("no `document` on window; aborting mount");
        return;
    };

    if document.get_element_by_id("content").is_none() {
        console_log("host HTML is missing a #content element; aborting mount");
        return;
    }

    wasm_bindgen_futures::spawn_local(async move {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Some(content) = document.get_element_by_id("content") else {
            return;
        };
        match api::health_check().await {
            Ok(health) => {
                content.set_text_content(Some(&format!(
                    "{} — {} v{}",
                    health.status, health.service, health.version
                )));
            }
            Err(e) => {
                content.set_text_content(Some(&format!("health check failed: {e}")));
            }
        }
    });
}

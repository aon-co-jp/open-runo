//! open-runo desktop frontend, compiled to WebAssembly.
//!
//! Replaces the former Tauri + TypeScript + Bootstrap + Node.js stack.
//! Rust is the only language for both frontend and backend; this crate
//! compiles to `wasm32-unknown-unknown` and runs directly in a webview or
//! browser via `wasm-bindgen`'s generated JS glue (a thin loader, not a
//! build toolchain — no webpack/vite/tsc in this crate's own pipeline).
//!
//! `invoke()`-style calls (the one thing Tauri provided that's worth
//! keeping compatibility with) are implemented in `api.rs` as plain async
//! functions calling the REST API directly via `web_sys::window().fetch()`,
//! rather than an IPC bridge to a separate host process.

use wasm_bindgen::prelude::*;

mod api;
mod notifications;
mod pages;

#[wasm_bindgen(start)]
pub fn start() {
    console_log("open-runo-desktop-wasm starting");
    pages::mount();
}

fn console_log(msg: &str) {
    web_sys::console::log_1(&JsValue::from_str(msg));
}

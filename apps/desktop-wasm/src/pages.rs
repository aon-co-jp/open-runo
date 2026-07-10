//! Page rendering + sidebar navigation. Poem/Tauri/Node-free equivalent of
//! the old `src/main.ts` router and `src/pages/*.ts` — content is set via
//! `innerHTML` (like the old templates) and forms are wired up with
//! `wasm_bindgen::Closure` event listeners instead of TypeScript handlers.

use crate::api;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};

fn document() -> web_sys::Document {
    web_sys::window().expect("no window").document().expect("no document")
}

fn content_el() -> Option<Element> {
    document().get_element_by_id("content")
}

fn by_id(id: &str) -> Option<Element> {
    document().get_element_by_id(id)
}

fn input_value(id: &str) -> String {
    by_id(id)
        .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default()
}

fn textarea_value(id: &str) -> String {
    by_id(id)
        .and_then(|e| e.dyn_into::<HtmlTextAreaElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default()
}

fn select_value(id: &str) -> String {
    by_id(id)
        .and_then(|e| e.dyn_into::<HtmlSelectElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default()
}

fn set_text(id: &str, text: &str) {
    if let Some(el) = by_id(id) {
        el.set_text_content(Some(text));
    }
}

/// Attach `on_click` to element `id`, leaking the closure (fine for a
/// page's lifetime — the whole page is torn down and its listeners
/// dropped together whenever `render_*` overwrites `#content`).
fn on_click(id: &str, on_click: impl Fn() + 'static) {
    let Some(el) = by_id(id) else { return };
    let closure = Closure::<dyn Fn()>::new(on_click);
    let _ = el.add_event_listener_with_callback(
        "click",
        closure.as_ref().unchecked_ref(),
    );
    closure.forget();
}

/// Mount the app: wire up sidebar navigation and render the initial page.
/// Equivalent to the old `main.ts`'s `navigate()` dispatcher.
pub fn mount() {
    let Some(content) = content_el() else {
        web_sys::console::log_1(&JsValue::from_str(
            "host HTML is missing a #content element; aborting mount",
        ));
        return;
    };
    let _ = content;

    for page in ["dashboard", "schemas", "federation", "ai-routing"] {
        let link_id = format!("nav-{page}");
        on_click(&link_id, move || navigate(page));
    }

    navigate("dashboard");
}

fn navigate(page: &str) {
    let document = document();
    if let Some(nav_list) = document.get_element_by_id("sidebar-nav") {
        let links = nav_list.query_selector_all("a").ok();
        if let Some(links) = links {
            for i in 0..links.length() {
                if let Some(node) = links.item(i) {
                    if let Ok(el) = node.dyn_into::<Element>() {
                        let is_active = el.id() == format!("nav-{page}");
                        let _ = el.set_attribute("data-active", if is_active { "true" } else { "false" });
                    }
                }
            }
        }
    }

    match page {
        "dashboard" => render_dashboard(),
        "schemas" => render_schemas(),
        "federation" => render_federation(),
        "ai-routing" => render_ai_routing(),
        _ => {}
    }
}

fn render_dashboard() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"<h2>Dashboard</h2><p id="health-status">Checking backend health…</p>"#,
    );
    wasm_bindgen_futures::spawn_local(async move {
        match api::health_check().await {
            Ok(h) => set_text(
                "health-status",
                &format!("{} — {} v{}", h.status, h.service, h.version),
            ),
            Err(e) => set_text("health-status", &format!("health check failed: {e}")),
        }
    });
}

fn render_schemas() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>Schema Registry</h2>
        <fieldset>
          <legend>Register Schema</legend>
          <label>Service name <input id="svc-name" placeholder="users-service" /></label><br/>
          <label>Stage
            <select id="svc-stage">
              <option value="local">local</option>
              <option value="development">development</option>
              <option value="staging">staging</option>
              <option value="production">production</option>
            </select>
          </label><br/>
          <label>SDL<br/><textarea id="svc-sdl" rows="4" placeholder="type User { id: ID! }"></textarea></label><br/>
          <button id="register-btn">Register</button>
          <span id="register-msg"></span>
        </fieldset>
        <fieldset>
          <legend>Schema History</legend>
          <input id="hist-svc" placeholder="service name" />
          <button id="hist-btn">Fetch</button>
          <pre id="history-list"></pre>
        </fieldset>
        "#,
    );

    on_click("register-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let name = input_value("svc-name");
            let sdl = textarea_value("svc-sdl");
            let stage = select_value("svc-stage");
            set_text("register-msg", "registering…");
            match api::register_schema(&name, &sdl, &stage).await {
                Ok(r) => set_text("register-msg", &format!("registered {} ({})", r.service_name, r.id)),
                Err(e) => set_text("register-msg", &format!("failed: {e}")),
            }
        });
    });

    on_click("hist-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            let service = input_value("hist-svc");
            set_text("history-list", "loading…");
            match api::get_schema_history(&service).await {
                Ok(h) => {
                    let lines: Vec<String> = h
                        .versions
                        .iter()
                        .map(|v| format!("{} [{}] {}", v.id, v.stage, v.created_at))
                        .collect();
                    set_text("history-list", &lines.join("\n"));
                }
                Err(e) => set_text("history-list", &format!("failed: {e}")),
            }
        });
    });
}

fn render_federation() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>Federation</h2>
        <button id="refresh-status-btn">Refresh status</button>
        <pre id="federation-status">Loading…</pre>
        "#,
    );

    let load = || {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("federation-status", "loading…");
            match api::federation_status().await {
                Ok(s) => set_text(
                    "federation-status",
                    &format!(
                        "contributing_services: {}\ntype_count: {}\nfield_count: {}",
                        s.contributing_services.join(", "),
                        s.type_count,
                        s.field_count
                    ),
                ),
                Err(e) => set_text("federation-status", &format!("failed: {e}")),
            }
        });
    };
    load();
    on_click("refresh-status-btn", load);
}

fn render_ai_routing() {
    let Some(content) = content_el() else { return };
    content.set_inner_html(
        r#"
        <h2>AI Routing</h2>
        <p>Picks the best provider between a local model and Anthropic Claude, cost-optimized.</p>
        <button id="route-btn">Route request</button>
        <pre id="route-result"></pre>
        "#,
    );

    on_click("route-btn", || {
        wasm_bindgen_futures::spawn_local(async move {
            set_text("route-result", "routing…");
            let candidates = vec![
                api::AiRouteCandidate {
                    provider: "local_llm",
                    estimated_cost_usd_per_1k_tokens: 0.0,
                    estimated_latency_ms: 900,
                    is_local: true,
                    context_length: 8000,
                },
                api::AiRouteCandidate {
                    provider: "anthropic",
                    estimated_cost_usd_per_1k_tokens: 3.0,
                    estimated_latency_ms: 400,
                    is_local: false,
                    context_length: 200_000,
                },
            ];
            match api::ai_route("cost", candidates).await {
                Ok(r) => set_text(
                    "route-result",
                    &format!(
                        "selected: {} (local={}, cost=${:.2}/1k, latency={}ms)",
                        r.selected_provider, r.is_local, r.estimated_cost_usd_per_1k_tokens, r.estimated_latency_ms
                    ),
                ),
                Err(e) => set_text("route-result", &format!("failed: {e}")),
            }
        });
    });
}

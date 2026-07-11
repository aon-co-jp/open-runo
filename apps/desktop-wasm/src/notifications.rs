//! Native OS notifications via the browser's Web Notifications API —
//! Tauri-parity gap ("ネイティブ通知", `docs/tauri-parity.md`). Genuine OS
//! notifications (Windows toast / macOS Notification Center / Linux
//! desktop notification), not an in-page toast — the browser hands the
//! rendering off to the OS, the same end-user-visible result Tauri's
//! notification plugin produces, without depending on the `tauri` package.
//!
//! Permission must be requested from a user gesture in most browsers, so
//! `request_permission()` is called once from a click handler on first use
//! (not unconditionally on page load, which browsers may silently ignore
//! or which annoys users with an unprompted permission dialog).

use wasm_bindgen_futures::JsFuture;
use web_sys::{Notification, NotificationOptions, NotificationPermission};

/// Ask the user for notification permission if we haven't already been
/// granted or denied it. Safe to call before every [`notify`] — cheap and
/// a no-op once the user has answered once (the browser remembers the
/// decision per-origin).
async fn ensure_permission() -> bool {
    match Notification::permission() {
        NotificationPermission::Granted => true,
        NotificationPermission::Denied => false,
        _ => {
            let Ok(promise) = Notification::request_permission() else {
                return false;
            };
            let Ok(result) = JsFuture::from(promise).await else {
                return false;
            };
            result.as_string().as_deref() == Some("granted")
        }
    }
}

/// Fire a native OS notification with `title`/`body`. Silently does
/// nothing if the browser doesn't support the Notifications API, or the
/// user has denied/not yet granted permission — this is a convenience
/// layer on top of the in-page status text every caller already sets, not
/// the only way results are surfaced, so failure here is never fatal to
/// the operation it's reporting on.
pub fn notify(title: &str, body: &str) {
    let title = title.to_string();
    let body = body.to_string();
    wasm_bindgen_futures::spawn_local(async move {
        if !ensure_permission().await {
            return;
        }
        let opts = NotificationOptions::new();
        opts.set_body(&body);
        let _ = Notification::new_with_options(&title, &opts);
    });
}

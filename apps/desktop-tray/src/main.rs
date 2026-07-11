//! open-runo native tray companion — Tauri-parity gap ("システムトレイ",
//! `docs/tauri-parity.md`). The WASM admin UI (`apps/desktop-wasm`) runs
//! entirely inside a browser tab, which has no way to place an icon in the
//! OS notification area — that's a genuine, structural limitation of
//! "browser execution" as a delivery model, not something WASM/JS can work
//! around. This binary is a separate, minimal native process that fills
//! exactly that gap: a tray icon with an "Open"/"Quit" menu, plus a native
//! startup notification, while the actual admin UI stays the browser-based
//! WASM app it already is (this binary has no UI of its own beyond the
//! tray icon and opens the real app in the user's default browser).
//!
//! Deliberately does **not** depend on the `tauri` crate (the app
//! framework this whole ecosystem avoids). It uses `tray-icon` (tray icon
//! abstraction) + `tao` (event loop / native message pump, required on
//! Windows for the tray icon to receive click events) + `notify-rust`
//! (native OS notifications). Note: `notify-rust`'s Windows backend pulls
//! in `tauri-winrt-notification` transitively — that crate is a narrow
//! WinRT toast-notification binding maintained under the Tauri GitHub
//! org, **not** the `tauri` app-framework crate itself (it provides no
//! IPC, no WebView bundling, no app builder — just "show a Windows
//! toast"), so this does not violate the "no `tauri` package dependency"
//! policy. Documented here explicitly so a future `cargo tree` grep for
//! "tauri" doesn't cause confusion.

use notify_rust::Notification;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

/// URL the tray icon opens in the user's default browser. Overridable so
/// this binary can point at a non-default port/host without a rebuild.
fn app_url() -> String {
    std::env::var("OPEN_RUNO_TRAY_URL").unwrap_or_else(|_| "http://localhost:8080/".to_string())
}

/// Open `url` in the OS default browser. Hand-rolled per-OS `Command`
/// invocation rather than pulling in the `open` crate — three
/// `Command::new(...).arg(...)` calls behind a `#[cfg]` is not worth a new
/// dependency for.
fn open_in_browser(url: &str) {
    let result = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = result {
        eprintln!("failed to open browser at {url}: {e}");
    }
}

/// Generate a small solid-square icon procedurally at build/run time
/// (RGBA, hand-computed) instead of shipping/loading an external image
/// asset — a simple two-tone square is enough to be visible and
/// recognizable in a system tray at 32x32, and avoids adding an image-
/// decoding crate for one static icon.
fn build_tray_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    // open-runo brand-ish color: a dark slate square with a lighter inset,
    // echoing apps/desktop-wasm/www/icon.svg's hexagon-on-dark palette
    // without needing an SVG rasterizer.
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inset = x >= 6 && x < SIZE - 6 && y >= 6 && y < SIZE - 6;
            let (r, g, b) = if inset { (0x4c, 0xa3, 0xff) } else { (0x14, 0x16, 0x1a) };
            rgba.extend_from_slice(&[r, g, b, 0xff]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("32x32 RGBA buffer is a valid icon")
}

enum TrayUserEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
}

fn main() {
    let event_loop = EventLoopBuilder::<TrayUserEvent>::with_user_event().build();

    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(TrayUserEvent::Tray(event));
    }));
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(TrayUserEvent::Menu(event));
    }));

    let menu = Menu::new();
    let open_item = MenuItem::new("Open open-runo", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append(&open_item).expect("append open menu item");
    menu.append(&PredefinedMenuItem::separator()).expect("append separator");
    menu.append(&quit_item).expect("append quit menu item");
    let open_item_id = open_item.id().clone();
    let quit_item_id = quit_item.id().clone();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(build_tray_icon())
        .with_tooltip("open-runo")
        .build()
        .expect("build tray icon");

    let _ = Notification::new()
        .summary("open-runo")
        .body("Tray companion running. Click the tray icon to open the admin UI.")
        .show();

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(TrayUserEvent::Menu(event)) => {
                if event.id == open_item_id {
                    open_in_browser(&app_url());
                } else if event.id == quit_item_id {
                    *control_flow = ControlFlow::Exit;
                }
            }
            // A plain left-click on the tray icon is the common "open the
            // app" gesture (matches typical Tauri/Electron tray behavior);
            // right-click is handled by the OS showing the menu itself.
            Event::UserEvent(TrayUserEvent::Tray(TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            })) => {
                open_in_browser(&app_url());
            }
            _ => {}
        }
    });
}

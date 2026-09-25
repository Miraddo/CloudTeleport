//! System tray / menu bar icon.
//!
//! On Windows and macOS the icon lives on the main (UI) thread. On Linux the
//! AppIndicator backend needs a GTK main loop, which runs on its own thread.

use std::sync::Arc;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::core::{Command, Core};

const OPEN: &str = "open";
const SYNC: &str = "sync";
const PAUSE: &str = "pause";
const QUIT: &str = "quit";

/// Keeps the tray icon alive (on platforms where it lives on the UI thread).
pub struct Tray {
    _icon: Option<TrayIcon>,
}

/// Creates the tray icon and routes its events to `core`. Must be called from the UI thread
/// after the event loop started.
pub fn start(core: Arc<Core>) -> Tray {
    install_handlers(core);

    #[cfg(target_os = "linux")]
    {
        std::thread::Builder::new()
            .name("tray".into())
            .spawn(|| {
                if let Err(e) = gtk::init() {
                    tracing::warn!("GTK init failed, no tray icon: {e}");
                    return;
                }
                match build() {
                    Ok(icon) => {
                        let _icon = icon;
                        gtk::main();
                    }
                    Err(e) => tracing::warn!("could not create the tray icon: {e}"),
                }
            })
            .expect("spawn tray thread");
        Tray { _icon: None }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let icon = build()
            .map_err(|e| tracing::warn!("could not create the tray icon: {e}"))
            .ok();
        Tray { _icon: icon }
    }
}

fn build() -> tray_icon::Result<TrayIcon> {
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id(OPEN, "Open CloudTeleport", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(SYNC, "Sync now", true, None),
        &MenuItem::with_id(PAUSE, "Pause / resume syncing", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(QUIT, "Quit", true, None),
    ])
    .expect("tray menu");

    TrayIconBuilder::new()
        .with_icon(crate::icon::tray_icon())
        .with_tooltip("CloudTeleport — Google Drive → Telegram")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
}

fn install_handlers(core: Arc<Core>) {
    let menu_core = core.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.as_ref() {
        OPEN => menu_core.show_window(),
        SYNC => menu_core.send(Command::SyncNow),
        PAUSE => menu_core.set_paused(!menu_core.is_paused()),
        QUIT => menu_core.quit(),
        _ => {}
    }));

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        let left_click = matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        );
        if left_click {
            core.show_window();
        }
    }));
}

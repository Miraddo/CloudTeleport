#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! CloudTeleport — forward files from Google Drive folders to Telegram chats,
//! with a desktop UI and a system tray icon that keeps it running in the background.

mod autostart;
mod caption;
mod config;
mod core;
mod google;
mod icon;
mod telegram;
mod tray;
mod ui;
mod worker;

use std::sync::Arc;

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cloudteleport=info".into()),
        )
        .init();

    let paths = config::Paths::new()?;
    let cfg: config::AppConfig = config::load(&paths.config);
    let start_hidden =
        cfg.start_minimized || std::env::args().any(|a| a == autostart::MINIMIZED_FLAG);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let core = Arc::new(core::Core::new(paths, cfg, runtime.handle().clone(), tx));
    runtime.spawn(worker::run(core.clone(), rx));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CloudTeleport")
            .with_app_id("cloudteleport")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([720.0, 480.0])
            .with_icon(icon::window_icon())
            .with_visible(!start_hidden),
        ..Default::default()
    };

    let app_core = core.clone();
    eframe::run_native(
        "CloudTeleport",
        options,
        Box::new(move |cc| {
            app_core.attach_ui(cc.egui_ctx.clone());
            let tray = tray::start(app_core.clone());
            Ok(Box::new(ui::App::new(cc, app_core, tray)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("UI error: {e}"))?;

    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    Ok(())
}

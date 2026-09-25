//! State shared between the UI, the tray icon and the background worker.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use chrono::{DateTime, Local};
use parking_lot::{Mutex, RwLock};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{self, AppConfig, Paths};
use crate::google::{DriveFile, Google};
use crate::telegram::Telegram;

const MAX_LOG: usize = 500;

/// Requests handled by the background worker.
pub enum Command {
    SyncNow,
    ConfigChanged,
    Send(ManualSend),
}

/// Files picked in the "Share files" page.
pub struct ManualSend {
    pub chat_id: String,
    pub files: Vec<DriveFile>,
    pub folder_name: String,
    pub delivery: config::Delivery,
    pub caption: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Error,
}

pub struct LogEntry {
    pub time: DateTime<Local>,
    pub level: Level,
    pub text: String,
}

#[derive(Default)]
pub struct Status {
    pub busy: Option<String>,
    pub last_sync: Option<DateTime<Local>>,
    pub next_sync: Option<DateTime<Local>>,
    pub total_sent: u64,
    /// Configuration problem that prevents syncing.
    pub problem: Option<String>,
    pub log: VecDeque<LogEntry>,
}

pub struct Core {
    pub paths: Paths,
    pub http: reqwest::Client,
    pub google: Google,
    pub config: RwLock<AppConfig>,
    pub status: Mutex<Status>,
    pub rt: tokio::runtime::Handle,
    pub quitting: AtomicBool,
    commands: UnboundedSender<Command>,
    ctx: OnceLock<egui::Context>,
}

impl Core {
    pub fn new(
        paths: Paths,
        config: AppConfig,
        rt: tokio::runtime::Handle,
        commands: UnboundedSender<Command>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("CloudTeleport/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("failed to build HTTP client");
        let google = Google::new(http.clone(), paths.tokens.clone());
        Self {
            paths,
            http,
            google,
            config: RwLock::new(config),
            status: Mutex::new(Status::default()),
            rt,
            quitting: AtomicBool::new(false),
            commands,
            ctx: OnceLock::new(),
        }
    }

    pub fn attach_ui(&self, ctx: egui::Context) {
        let _ = self.ctx.set(ctx);
    }

    pub fn repaint(&self) {
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }

    pub fn log(&self, level: Level, text: impl Into<String>) {
        let text = text.into();
        match level {
            Level::Error => tracing::error!("{text}"),
            _ => tracing::info!("{text}"),
        }
        let mut status = self.status.lock();
        status.log.push_front(LogEntry {
            time: Local::now(),
            level,
            text,
        });
        status.log.truncate(MAX_LOG);
        drop(status);
        self.repaint();
    }

    /// Applies a change to the configuration, saves it and notifies the worker.
    pub fn update_config(&self, change: impl FnOnce(&mut AppConfig)) {
        let snapshot = {
            let mut cfg = self.config.write();
            change(&mut cfg);
            cfg.clone()
        };
        if let Err(e) = config::save(&self.paths.config, &snapshot) {
            self.log(Level::Error, format!("Could not save settings: {e:#}"));
        }
        self.send(Command::ConfigChanged);
        self.repaint();
    }

    pub fn is_paused(&self) -> bool {
        self.config.read().paused
    }

    pub fn set_paused(&self, paused: bool) {
        self.update_config(|c| c.paused = paused);
        self.log(
            Level::Info,
            if paused {
                "Syncing paused"
            } else {
                "Syncing resumed"
            },
        );
    }

    pub fn telegram(&self) -> Result<Telegram> {
        Telegram::new(self.http.clone(), &self.config.read().telegram)
    }

    /// Shows and focuses the main window.
    pub fn show_window(&self) {
        if let Some(ctx) = self.ctx.get() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    }

    pub fn hide_window(&self) {
        if let Some(ctx) = self.ctx.get() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    /// Exits the application (the window close request is no longer intercepted).
    pub fn quit(&self) {
        self.quitting.store(true, Ordering::SeqCst);
        if let Some(ctx) = self.ctx.get() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx.request_repaint();
        }
        // Fallback in case the (hidden) window does not process the close request.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(3));
            std::process::exit(0);
        });
    }

    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    /// Runs a future on the background runtime; poll the returned [`Task`] from the UI.
    pub fn spawn<T, F>(self: &Arc<Self>, fut: F) -> Task<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let slot = Arc::new(Mutex::new(None));
        let out = slot.clone();
        let core = self.clone();
        self.rt.spawn(async move {
            let result = fut.await;
            *out.lock() = Some(result);
            core.repaint();
        });
        Task(slot)
    }
}

/// Result of a background operation started from the UI.
pub struct Task<T>(Arc<Mutex<Option<Result<T>>>>);

impl<T> Task<T> {
    /// Returns the result once, when the operation has finished.
    pub fn take(&self) -> Option<Result<T>> {
        self.0.lock().take()
    }
}

/// Polls an optional task; clears it and returns the result when done.
pub fn poll<T>(task: &mut Option<Task<T>>) -> Option<Result<T>> {
    let result = task.as_ref()?.take()?;
    *task = None;
    Some(result)
}

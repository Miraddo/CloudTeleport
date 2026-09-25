//! Background loop: watches Drive folders and forwards new files to Telegram.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Local;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{sleep, sleep_until, Instant};

use crate::caption;
use crate::config::{self, AppConfig, Delivery, Route, SyncState};
use crate::core::{Command, Core, Level, ManualSend};
use crate::google::DriveFile;
use crate::telegram::Telegram;

/// A file is skipped after failing this many times in a row.
const MAX_ATTEMPTS: u32 = 3;
/// Pause between two uploads to stay below Telegram's rate limits.
const SEND_DELAY: Duration = Duration::from_millis(1200);

pub async fn run(core: Arc<Core>, mut commands: UnboundedReceiver<Command>) {
    let mut worker = Worker {
        state: config::load(&core.paths.state),
        failures: HashMap::new(),
        core,
    };
    worker.core.status.lock().total_sent = worker.state.total_sent;

    let mut next = Instant::now() + Duration::from_secs(3);
    loop {
        worker.set_next_sync(next);
        tokio::select! {
            _ = sleep_until(next) => {
                if !worker.core.is_paused() {
                    worker.sync_all().await;
                }
                next = Instant::now() + worker.interval();
            }
            cmd = commands.recv() => match cmd {
                None => break,
                Some(Command::SyncNow) => {
                    worker.sync_all().await;
                    next = Instant::now() + worker.interval();
                }
                Some(Command::ConfigChanged) => {
                    next = next.min(Instant::now() + worker.interval());
                }
                Some(Command::Send(request)) => worker.manual_send(request).await,
            }
        }
    }
}

struct Worker {
    core: Arc<Core>,
    state: SyncState,
    /// Consecutive failures per `route id/file id`.
    failures: HashMap<String, u32>,
}

impl Worker {
    fn interval(&self) -> Duration {
        Duration::from_secs(self.core.config.read().poll_interval_secs.max(15))
    }

    fn set_next_sync(&self, next: Instant) {
        let paused = self.core.is_paused();
        let mut status = self.core.status.lock();
        status.next_sync = (!paused).then(|| {
            Local::now() + chrono::Duration::from_std(next - Instant::now()).unwrap_or_default()
        });
        drop(status);
        self.core.repaint();
    }

    fn set_busy(&self, busy: Option<String>) {
        self.core.status.lock().busy = busy;
        self.core.repaint();
    }

    fn save_state(&self) {
        if let Err(e) = config::save(&self.core.paths.state, &self.state) {
            self.core
                .log(Level::Error, format!("Could not save sync state: {e:#}"));
        }
    }

    /// Checks whether the app is configured well enough to sync.
    async fn preflight(&self, cfg: &AppConfig) -> Result<Telegram, String> {
        if !self.core.google.is_signed_in().await {
            return Err("Sign in to Google in Settings".into());
        }
        self.core
            .telegram()
            .map_err(|_| "Enter your Telegram bot token in Settings".to_string())
            .and_then(|tg| {
                if cfg.routes.iter().any(|r| r.enabled) {
                    Ok(tg)
                } else {
                    Err("Add a route to start forwarding files".into())
                }
            })
    }

    async fn sync_all(&mut self) {
        let cfg = self.core.config.read().clone();
        let tg = match self.preflight(&cfg).await {
            Ok(tg) => tg,
            Err(problem) => {
                self.core.status.lock().problem = Some(problem);
                self.core.repaint();
                return;
            }
        };
        self.core.status.lock().problem = None;

        // Forget state of deleted routes.
        let before = self.state.routes.len();
        self.state
            .routes
            .retain(|id, _| cfg.routes.iter().any(|r| &r.id == id));
        if self.state.routes.len() != before {
            self.save_state();
        }

        for route in cfg.routes.iter().filter(|r| r.enabled) {
            if route.folder_id.is_empty() || route.chat_id.trim().is_empty() {
                continue;
            }
            self.set_busy(Some(format!("Checking “{}”", route_label(route))));
            if let Err(e) = self.sync_route(&cfg, &tg, route).await {
                self.core
                    .log(Level::Error, format!("{}: {e:#}", route_label(route)));
            }
        }
        self.set_busy(None);
        self.core.status.lock().last_sync = Some(Local::now());
        self.core.repaint();
    }

    async fn sync_route(&mut self, cfg: &AppConfig, tg: &Telegram, route: &Route) -> Result<()> {
        let files: Vec<DriveFile> = self
            .core
            .google
            .list_folder(&cfg.google, &route.folder_id, false)
            .await
            .context("listing the Drive folder")?
            .into_iter()
            .filter(|f| !f.is_folder())
            .collect();

        let rs = self.state.routes.entry(route.id.clone()).or_default();
        if rs.folder_id != route.folder_id {
            *rs = config::RouteState {
                folder_id: route.folder_id.clone(),
                ..Default::default()
            };
        }
        if !rs.initialized {
            rs.initialized = true;
            if !route.include_existing {
                rs.seen = files.iter().map(|f| f.id.clone()).collect();
                let count = rs.seen.len();
                self.save_state();
                self.core.log(
                    Level::Info,
                    format!(
                        "{}: now watching “{}” ({count} existing files skipped)",
                        route_label(route),
                        route.folder_name
                    ),
                );
                return Ok(());
            }
        }
        // Forget files that left the folder, so the state does not grow forever.
        let current: HashSet<&str> = files.iter().map(|f| f.id.as_str()).collect();
        rs.seen.retain(|id| current.contains(id.as_str()));

        let pending: Vec<&DriveFile> = files.iter().filter(|f| !rs.seen.contains(&f.id)).collect();
        for (i, file) in pending.iter().enumerate() {
            if self.core.is_quitting() {
                break;
            }
            self.set_busy(Some(format!(
                "Sending {} of {}: {}",
                i + 1,
                pending.len(),
                file.name
            )));
            let key = format!("{}/{}", route.id, file.id);
            let caption = caption::render(&route.caption, file, &route.folder_name);
            match deliver(
                &self.core,
                cfg,
                tg,
                &route.chat_id,
                route.delivery,
                &caption,
                file,
            )
            .await
            {
                Ok(how) => {
                    self.failures.remove(&key);
                    self.mark_seen(&route.id, &file.id, true);
                    self.core.log(
                        Level::Success,
                        format!("{}: {how} “{}”", route_label(route), file.name),
                    );
                }
                Err(e) => {
                    let attempts = self.failures.entry(key.clone()).or_insert(0);
                    *attempts += 1;
                    let give_up = *attempts >= MAX_ATTEMPTS;
                    self.core.log(
                        Level::Error,
                        format!(
                            "{}: could not send “{}”: {e:#}{}",
                            route_label(route),
                            file.name,
                            if give_up {
                                " — giving up on this file"
                            } else {
                                " — will retry"
                            }
                        ),
                    );
                    if give_up {
                        self.failures.remove(&key);
                        self.mark_seen(&route.id, &file.id, false);
                    }
                }
            }
            sleep(SEND_DELAY).await;
        }
        Ok(())
    }

    fn mark_seen(&mut self, route_id: &str, file_id: &str, sent: bool) {
        if let Some(rs) = self.state.routes.get_mut(route_id) {
            rs.seen.insert(file_id.to_string());
        }
        if sent {
            self.state.total_sent += 1;
            self.core.status.lock().total_sent = self.state.total_sent;
        }
        self.save_state();
    }

    async fn manual_send(&mut self, request: ManualSend) {
        let cfg = self.core.config.read().clone();
        let tg = match self.core.telegram() {
            Ok(tg) => tg,
            Err(e) => return self.core.log(Level::Error, format!("{e:#}")),
        };
        let total = request.files.len();
        let mut sent = 0;
        for (i, file) in request.files.iter().enumerate() {
            self.set_busy(Some(format!("Sending {} of {total}: {}", i + 1, file.name)));
            let caption = caption::render(&request.caption, file, &request.folder_name);
            match deliver(
                &self.core,
                &cfg,
                &tg,
                &request.chat_id,
                request.delivery,
                &caption,
                file,
            )
            .await
            {
                Ok(how) => {
                    sent += 1;
                    self.core.log(
                        Level::Success,
                        format!("{how} “{}” → {}", file.name, request.chat_id),
                    );
                }
                Err(e) => self.core.log(
                    Level::Error,
                    format!("Could not send “{}”: {e:#}", file.name),
                ),
            }
            if i + 1 < total {
                sleep(SEND_DELAY).await;
            }
        }
        self.state.total_sent += sent;
        self.core.status.lock().total_sent = self.state.total_sent;
        self.save_state();
        self.set_busy(None);
    }
}

fn route_label(route: &Route) -> &str {
    if route.name.trim().is_empty() {
        &route.folder_name
    } else {
        &route.name
    }
}

/// Sends one Drive file to a chat. Returns a short description of what was done.
async fn deliver(
    core: &Core,
    cfg: &AppConfig,
    tg: &Telegram,
    chat_id: &str,
    delivery: Delivery,
    caption: &str,
    file: &DriveFile,
) -> Result<&'static str> {
    let max_bytes = cfg.telegram.max_upload_mb.max(1) * 1024 * 1024;
    let too_big = file.size.is_some_and(|s| s > max_bytes);

    if delivery != Delivery::Link && !too_big {
        if let Some(download) = core.google.download(&cfg.google, file).await? {
            if download.bytes.len() as u64 <= max_bytes {
                tg.send_file(
                    chat_id,
                    &download.file_name,
                    &download.mime_type,
                    download.bytes,
                    caption,
                    delivery == Delivery::Media,
                )
                .await?;
                return Ok("sent");
            }
        }
    }

    let link = file.link();
    let mut text = caption.to_string();
    if !text.contains(&link) {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&link);
    }
    tg.send_message(chat_id, &text).await?;
    Ok(if delivery == Delivery::Link {
        "shared link for"
    } else {
        "shared link (too large or not downloadable) for"
    })
}

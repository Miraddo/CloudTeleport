//! Persistent configuration, credentials and sync state.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Telegram's Bot API refuses uploads larger than this (unless a local Bot API server is used).
pub const DEFAULT_MAX_UPLOAD_MB: u64 = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub google: GoogleConfig,
    pub telegram: TelegramConfig,
    pub routes: Vec<Route>,
    /// How often watched Drive folders are checked for new files.
    pub poll_interval_secs: u64,
    /// Start hidden in the system tray.
    pub start_minimized: bool,
    /// Launch automatically when the user logs in.
    pub autostart: bool,
    /// Closing the window keeps the app running in the tray instead of quitting.
    pub close_to_tray: bool,
    /// Automatic syncing is paused.
    pub paused: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            google: GoogleConfig::default(),
            telegram: TelegramConfig::default(),
            routes: Vec::new(),
            poll_interval_secs: 60,
            start_minimized: false,
            autostart: false,
            close_to_tray: true,
            paused: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GoogleConfig {
    /// OAuth client ID of a "Desktop app" OAuth client from Google Cloud Console.
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub bot_token: String,
    /// Base URL of the Bot API; change it to use a self-hosted Bot API server.
    pub api_base: String,
    /// Files larger than this are shared as a Drive link instead of being uploaded.
    pub max_upload_mb: u64,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            api_base: "https://api.telegram.org".into(),
            max_upload_mb: DEFAULT_MAX_UPLOAD_MB,
        }
    }
}

/// How a Drive file is delivered to Telegram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Delivery {
    /// Upload the file as a document (original quality, any type).
    #[default]
    Document,
    /// Upload photos/videos/audio as native Telegram media, everything else as a document.
    Media,
    /// Post a message with the Drive link only.
    Link,
}

impl Delivery {
    pub const ALL: [Delivery; 3] = [Delivery::Document, Delivery::Media, Delivery::Link];

    pub fn label(self) -> &'static str {
        match self {
            Delivery::Document => "Upload as file",
            Delivery::Media => "Upload as photo / video / audio",
            Delivery::Link => "Post Drive link only",
        }
    }
}

/// A watched Drive folder whose new files are forwarded to a Telegram chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Route {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub folder_id: String,
    pub folder_name: String,
    /// Numeric chat id (`-100…` for channels/supergroups) or `@channel_username`.
    pub chat_id: String,
    pub delivery: Delivery,
    /// Caption template, see [`crate::caption::PLACEHOLDERS`].
    pub caption: String,
    /// When the route is first run, also send the files already in the folder.
    pub include_existing: bool,
}

impl Default for Route {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            enabled: true,
            folder_id: String::new(),
            folder_name: String::new(),
            chat_id: String::new(),
            delivery: Delivery::Document,
            caption: "📄 {name}\n{link}".into(),
            include_existing: false,
        }
    }
}

/// OAuth tokens for the signed-in Google account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub account: String,
    /// OAuth client that issued the tokens.
    #[serde(default)]
    pub client_id: String,
}

/// What has already been forwarded, per route.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncState {
    pub routes: HashMap<String, RouteState>,
    pub total_sent: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RouteState {
    /// Set once the route has been scanned for the first time.
    pub initialized: bool,
    /// Drive file ids that were already handled.
    pub seen: HashSet<String>,
    /// Folder the state belongs to; changing a route's folder resets its state.
    pub folder_id: String,
}

pub struct Paths {
    pub config: PathBuf,
    pub tokens: PathBuf,
    pub state: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "CloudTeleport", "CloudTeleport")
            .context("could not determine the configuration directory")?;
        let dir = dirs.config_dir();
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self {
            config: dir.join("config.json"),
            tokens: dir.join("google_tokens.json"),
            state: dir.join("state.json"),
        })
    }
}

pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("ignoring unreadable {}: {e}", path.display());
            T::default()
        }),
        Err(_) => T::default(),
    }
}

pub fn load_opt<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Atomically writes `value` as JSON, readable only by the current user (it may hold secrets).
pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(value)?;
    write_private(&tmp, &data).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data)
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    fs::write(path, data)
}

pub fn delete(path: &Path) {
    let _ = fs::remove_file(path);
}

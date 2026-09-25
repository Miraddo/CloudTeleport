//! Google OAuth (installed-app loopback flow with PKCE) and the Drive v3 API.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::{self, GoogleConfig, GoogleTokens};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DRIVE_API: &str = "https://www.googleapis.com/drive/v3";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// Pseudo folder id used by the browser for "Shared with me".
pub const SHARED_WITH_ME: &str = "sharedWithMe";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    #[serde(default, deserialize_with = "de_opt_u64")]
    pub size: Option<u64>,
    pub created_time: Option<DateTime<Utc>>,
    pub modified_time: Option<DateTime<Utc>>,
    pub web_view_link: Option<String>,
}

impl DriveFile {
    pub fn is_folder(&self) -> bool {
        self.mime_type == FOLDER_MIME
    }

    pub fn link(&self) -> String {
        self.web_view_link
            .clone()
            .unwrap_or_else(|| format!("https://drive.google.com/file/d/{}/view", self.id))
    }
}

/// Drive returns `size` as a string.
fn de_opt_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.and_then(|s| s.parse().ok()))
}

/// Content of a downloaded (or exported) file.
pub struct Download {
    pub file_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct FileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

/// Holds the credentials and signs Drive API requests, refreshing the access token as needed.
pub struct Google {
    http: reqwest::Client,
    tokens_path: PathBuf,
    tokens: tokio::sync::Mutex<Option<GoogleTokens>>,
    /// Cached so the UI can read it without waiting for a token refresh.
    account: parking_lot::Mutex<Option<String>>,
}

impl Google {
    pub fn new(http: reqwest::Client, tokens_path: PathBuf) -> Self {
        let tokens: Option<GoogleTokens> = config::load_opt(&tokens_path);
        let account = parking_lot::Mutex::new(tokens.as_ref().map(|t| t.account.clone()));
        Self {
            http,
            tokens_path,
            tokens: tokio::sync::Mutex::new(tokens),
            account,
        }
    }

    /// The e-mail of the signed-in account, if any.
    pub fn account(&self) -> Option<String> {
        self.account.lock().clone()
    }

    pub async fn is_signed_in(&self) -> bool {
        self.tokens.lock().await.is_some()
    }

    pub async fn sign_out(&self) {
        *self.tokens.lock().await = None;
        *self.account.lock() = None;
        config::delete(&self.tokens_path);
    }

    /// Runs the browser consent flow and stores the resulting tokens. Returns the account e-mail.
    pub async fn sign_in(&self, cfg: &GoogleConfig) -> Result<String> {
        if cfg.client_id.trim().is_empty() {
            bail!("enter the Google OAuth client ID in Settings first");
        }
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());

        let verifier = random_string(64);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let state = random_string(24);

        let auth_url = url::Url::parse_with_params(
            AUTH_URL,
            &[
                ("client_id", cfg.client_id.trim()),
                ("redirect_uri", &redirect_uri),
                ("response_type", "code"),
                ("scope", SCOPE),
                ("access_type", "offline"),
                ("prompt", "consent"),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
            ],
        )?;
        open::that(auth_url.as_str()).context("could not open the web browser")?;

        let code = tokio::time::timeout(Duration::from_secs(300), wait_for_code(&listener, &state))
            .await
            .map_err(|_| anyhow!("timed out waiting for the Google sign-in"))??;

        let resp: TokenResponse = check(
            self.http
                .post(TOKEN_URL)
                .form(&[
                    ("code", code.as_str()),
                    ("client_id", cfg.client_id.trim()),
                    ("client_secret", cfg.client_secret.trim()),
                    ("redirect_uri", &redirect_uri),
                    ("grant_type", "authorization_code"),
                    ("code_verifier", &verifier),
                ])
                .send()
                .await?,
        )
        .await?
        .json()
        .await?;

        let refresh_token = resp
            .refresh_token
            .ok_or_else(|| anyhow!("Google did not return a refresh token"))?;
        let mut tokens = GoogleTokens {
            access_token: resp.access_token,
            refresh_token,
            expires_at: Utc::now() + chrono::Duration::seconds(resp.expires_in - 60),
            account: String::new(),
        };
        tokens.account = self
            .fetch_account(&tokens.access_token)
            .await
            .unwrap_or_else(|_| "your Google account".into());
        config::save(&self.tokens_path, &tokens)?;
        let account = tokens.account.clone();
        *self.tokens.lock().await = Some(tokens);
        *self.account.lock() = Some(account.clone());
        Ok(account)
    }

    async fn fetch_account(&self, access_token: &str) -> Result<String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct User {
            email_address: Option<String>,
            display_name: Option<String>,
        }
        #[derive(Deserialize)]
        struct About {
            user: User,
        }
        let about: About = check(
            self.http
                .get(format!("{DRIVE_API}/about"))
                .query(&[("fields", "user(emailAddress,displayName)")])
                .bearer_auth(access_token)
                .send()
                .await?,
        )
        .await?
        .json()
        .await?;
        Ok(about
            .user
            .email_address
            .or(about.user.display_name)
            .unwrap_or_default())
    }

    /// Returns a valid access token, refreshing it if it expired.
    async fn access_token(&self, cfg: &GoogleConfig) -> Result<String> {
        let mut guard = self.tokens.lock().await;
        let tokens = guard
            .as_mut()
            .ok_or_else(|| anyhow!("not signed in to Google"))?;
        if tokens.expires_at > Utc::now() {
            return Ok(tokens.access_token.clone());
        }
        let resp: TokenResponse = check(
            self.http
                .post(TOKEN_URL)
                .form(&[
                    ("client_id", cfg.client_id.trim()),
                    ("client_secret", cfg.client_secret.trim()),
                    ("refresh_token", tokens.refresh_token.as_str()),
                    ("grant_type", "refresh_token"),
                ])
                .send()
                .await?,
        )
        .await
        .context("refreshing the Google access token (try signing in again)")?
        .json()
        .await?;
        tokens.access_token = resp.access_token;
        tokens.expires_at = Utc::now() + chrono::Duration::seconds(resp.expires_in - 60);
        if let Some(rt) = resp.refresh_token {
            tokens.refresh_token = rt;
        }
        config::save(&self.tokens_path, &*tokens)?;
        Ok(tokens.access_token.clone())
    }

    /// Lists the non-trashed children of a folder (`"root"` for My Drive, or
    /// [`SHARED_WITH_ME`]). `folders_only` restricts the result to sub-folders.
    pub async fn list_folder(
        &self,
        cfg: &GoogleConfig,
        folder_id: &str,
        folders_only: bool,
    ) -> Result<Vec<DriveFile>> {
        let mut q = if folder_id == SHARED_WITH_ME {
            "sharedWithMe = true and trashed = false".to_string()
        } else {
            format!(
                "'{}' in parents and trashed = false",
                folder_id.replace('\'', "\\'")
            )
        };
        if folders_only {
            q.push_str(&format!(" and mimeType = '{FOLDER_MIME}'"));
        }
        let mut files = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = self.access_token(cfg).await?;
            let mut req = self
                .http
                .get(format!("{DRIVE_API}/files"))
                .bearer_auth(token)
                .query(&[
                    ("q", q.as_str()),
                    (
                        "fields",
                        "nextPageToken,files(id,name,mimeType,size,createdTime,modifiedTime,webViewLink)",
                    ),
                    ("pageSize", "1000"),
                    ("orderBy", "folder,createdTime"),
                    ("supportsAllDrives", "true"),
                    ("includeItemsFromAllDrives", "true"),
                ]);
            if let Some(pt) = &page_token {
                req = req.query(&[("pageToken", pt)]);
            }
            let list: FileList = check(req.send().await?).await?.json().await?;
            files.extend(list.files);
            match list.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }
        Ok(files)
    }

    /// Resolves a folder id to its name.
    pub async fn folder_name(&self, cfg: &GoogleConfig, folder_id: &str) -> Result<String> {
        if folder_id == "root" {
            return Ok("My Drive".into());
        }
        #[derive(Deserialize)]
        struct Named {
            name: String,
        }
        let token = self.access_token(cfg).await?;
        let file: Named = check(
            self.http
                .get(format!("{DRIVE_API}/files/{folder_id}"))
                .bearer_auth(token)
                .query(&[("fields", "name"), ("supportsAllDrives", "true")])
                .send()
                .await?,
        )
        .await?
        .json()
        .await?;
        Ok(file.name)
    }

    /// Downloads a file. Google Docs/Sheets/Slides/Drawings are exported to a regular format.
    /// Returns `None` for items that cannot be downloaded (e.g. Google Forms, shortcuts).
    pub async fn download(&self, cfg: &GoogleConfig, file: &DriveFile) -> Result<Option<Download>> {
        let token = self.access_token(cfg).await?;
        let (req, file_name, mime_type) = if let Some((mime, ext)) = export_format(&file.mime_type)
        {
            let req = self
                .http
                .get(format!("{DRIVE_API}/files/{}/export", file.id))
                .query(&[("mimeType", mime)]);
            (req, format!("{}.{ext}", file.name), mime.to_string())
        } else if file.mime_type.starts_with("application/vnd.google-apps.") {
            return Ok(None);
        } else {
            let req = self
                .http
                .get(format!("{DRIVE_API}/files/{}", file.id))
                .query(&[("alt", "media"), ("supportsAllDrives", "true")]);
            (req, file.name.clone(), file.mime_type.clone())
        };
        let bytes = check(req.bearer_auth(token).send().await?)
            .await?
            .bytes()
            .await?
            .to_vec();
        Ok(Some(Download {
            file_name,
            mime_type,
            bytes,
        }))
    }
}

/// Export target for Google Workspace documents.
fn export_format(mime: &str) -> Option<(&'static str, &'static str)> {
    Some(match mime {
        "application/vnd.google-apps.document" => ("application/pdf", "pdf"),
        "application/vnd.google-apps.presentation" => ("application/pdf", "pdf"),
        "application/vnd.google-apps.spreadsheet" => (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xlsx",
        ),
        "application/vnd.google-apps.drawing" => ("image/png", "png"),
        _ => return None,
    })
}

/// Extracts a folder id from a Drive URL, or returns the trimmed input.
pub fn parse_folder_id(input: &str) -> String {
    let input = input.trim();
    if let Ok(url) = url::Url::parse(input) {
        if let Some(pos) = url.path().find("/folders/") {
            let rest = &url.path()[pos + "/folders/".len()..];
            return rest.split('/').next().unwrap_or_default().to_string();
        }
        if let Some((_, id)) = url.query_pairs().find(|(k, _)| k == "id") {
            return id.into_owned();
        }
    }
    input.to_string()
}

async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);
        let path = request.split_whitespace().nth(1).unwrap_or("/");
        let url = url::Url::parse(&format!("http://localhost{path}"))?;
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };

        let (result, message) = match (param("code"), param("error")) {
            (Some(code), _) if param("state").as_deref() == Some(expected_state) => (
                Some(Ok(code)),
                "Signed in! You can close this tab and return to CloudTeleport.",
            ),
            (_, Some(err)) => (
                Some(Err(anyhow!("Google sign-in failed: {err}"))),
                "Sign-in failed. You can close this tab.",
            ),
            // Favicon requests etc.
            _ => (None, "Waiting for Google sign-in…"),
        };
        let body = format!(
            "<!doctype html><html><head><meta charset=utf-8><title>CloudTeleport</title></head>\
             <body style=\"font-family:sans-serif;text-align:center;margin-top:4em\"><h2>{message}</h2></body></html>"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        if let Some(result) = result {
            return result;
        }
    }
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error_description"))
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str().map(str::to_owned))
        })
        .unwrap_or(body);
    bail!("Google API error ({status}): {message}")
}

fn random_string(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::rng();
    (0..len)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_folder_urls() {
        assert_eq!(
            parse_folder_id("https://drive.google.com/drive/folders/1AbC_d-E?usp=sharing"),
            "1AbC_d-E"
        );
        assert_eq!(
            parse_folder_id("https://drive.google.com/drive/u/0/folders/XYZ"),
            "XYZ"
        );
        assert_eq!(
            parse_folder_id("https://drive.google.com/open?id=QQQ"),
            "QQQ"
        );
        assert_eq!(parse_folder_id("  rawId123 "), "rawId123");
    }

    #[test]
    fn deserializes_drive_file() {
        let f: DriveFile = serde_json::from_str(
            r#"{"id":"1","name":"a.png","mimeType":"image/png","size":"42","createdTime":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(f.size, Some(42));
        assert!(!f.is_folder());
        assert_eq!(f.link(), "https://drive.google.com/file/d/1/view");
    }
}

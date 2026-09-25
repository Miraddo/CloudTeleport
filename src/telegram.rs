//! Minimal Telegram Bot API client.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use reqwest::multipart::{Form, Part};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::config::TelegramConfig;

/// Telegram limits captions to 1024 characters and messages to 4096.
const MAX_CAPTION: usize = 1024;
const MAX_MESSAGE: usize = 4096;
/// `sendPhoto` rejects photos above 10 MB.
const MAX_PHOTO: usize = 10 * 1024 * 1024;

#[derive(Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    parameters: Option<ResponseParameters>,
}

#[derive(Deserialize)]
struct ResponseParameters {
    retry_after: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: Option<String>,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
}

impl Chat {
    pub fn display_name(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        let name = [self.first_name.as_deref(), self.last_name.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        match &self.username {
            Some(u) if name.is_empty() => format!("@{u}"),
            Some(u) => format!("{name} (@{u})"),
            None => name,
        }
    }
}

#[derive(Deserialize)]
struct User {
    username: Option<String>,
    first_name: String,
}

#[derive(Deserialize)]
struct Update {
    message: Option<Message>,
    channel_post: Option<Message>,
    my_chat_member: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    chat: Chat,
}

/// The Telegram method and form field used for an upload.
#[derive(Clone, Copy)]
enum UploadKind {
    Document,
    Photo,
    Video,
    Audio,
}

impl UploadKind {
    fn for_media(mime: &str, size: usize) -> Self {
        match mime {
            "image/jpeg" | "image/png" | "image/webp" if size <= MAX_PHOTO => Self::Photo,
            "video/mp4" => Self::Video,
            "audio/mpeg" | "audio/mp4" | "audio/x-m4a" => Self::Audio,
            _ => Self::Document,
        }
    }

    fn method_and_field(self) -> (&'static str, &'static str) {
        match self {
            Self::Document => ("sendDocument", "document"),
            Self::Photo => ("sendPhoto", "photo"),
            Self::Video => ("sendVideo", "video"),
            Self::Audio => ("sendAudio", "audio"),
        }
    }
}

pub struct Telegram {
    http: reqwest::Client,
    base: String,
}

impl Telegram {
    pub fn new(http: reqwest::Client, cfg: &TelegramConfig) -> Result<Self> {
        let token = cfg.bot_token.trim();
        if token.is_empty() {
            bail!("the Telegram bot token is not set");
        }
        let api = cfg.api_base.trim().trim_end_matches('/');
        let api = if api.is_empty() {
            "https://api.telegram.org"
        } else {
            api
        };
        Ok(Self {
            http,
            base: format!("{api}/bot{token}"),
        })
    }

    /// Returns the bot's `@username`.
    pub async fn get_me(&self) -> Result<String> {
        let me: User = self
            .call("getMe", || self.http.get(self.url("getMe")))
            .await?;
        Ok(me
            .username
            .map(|u| format!("@{u}"))
            .unwrap_or(me.first_name))
    }

    /// Chats the bot recently received messages from. Useful to find numeric chat ids:
    /// add the bot to a group/channel (or message it) and then call this.
    pub async fn recent_chats(&self) -> Result<Vec<Chat>> {
        let updates: Vec<Update> = self
            .call("getUpdates", || {
                self.http
                    .get(self.url("getUpdates"))
                    .query(&[("limit", "100"), ("timeout", "0")])
            })
            .await?;
        let mut chats: Vec<Chat> = Vec::new();
        for update in updates.into_iter().rev() {
            for msg in [update.message, update.channel_post, update.my_chat_member]
                .into_iter()
                .flatten()
            {
                if !chats.iter().any(|c| c.id == msg.chat.id) {
                    chats.push(msg.chat);
                }
            }
        }
        Ok(chats)
    }

    pub async fn send_message(&self, chat_id: &str, text: &str) -> Result<()> {
        let text = truncate(text, MAX_MESSAGE);
        let _: serde_json::Value = self
            .call("sendMessage", || {
                self.http
                    .post(self.url("sendMessage"))
                    .form(&[("chat_id", chat_id.trim()), ("text", text.as_str())])
            })
            .await?;
        Ok(())
    }

    /// Uploads a file. With `as_media`, photos, videos and audio use the native Telegram types.
    pub async fn send_file(
        &self,
        chat_id: &str,
        file_name: &str,
        mime_type: &str,
        bytes: Vec<u8>,
        caption: &str,
        as_media: bool,
    ) -> Result<()> {
        let kind = if as_media {
            UploadKind::for_media(mime_type, bytes.len())
        } else {
            UploadKind::Document
        };
        let (method, field) = kind.method_and_field();
        let caption = truncate(caption, MAX_CAPTION);
        let _: serde_json::Value = self
            .call(method, || {
                let part = Part::bytes(bytes.clone())
                    .file_name(file_name.to_string())
                    .mime_str(mime_type)
                    .unwrap_or_else(|_| {
                        Part::bytes(bytes.clone()).file_name(file_name.to_string())
                    });
                let mut form = Form::new()
                    .text("chat_id", chat_id.trim().to_string())
                    .part(field, part);
                if !caption.is_empty() {
                    form = form.text("caption", caption.clone());
                }
                self.http.post(self.url(method)).multipart(form)
            })
            .await?;
        Ok(())
    }

    fn url(&self, method: &str) -> String {
        format!("{}/{method}", self.base)
    }

    /// Sends a request, retrying when Telegram asks us to slow down (HTTP 429).
    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<T> {
        for _ in 0..4 {
            let resp = build()
                .send()
                .await
                .map_err(|e| anyhow!("Telegram {method} failed: {}", e.without_url()))?;
            let body: ApiResponse<T> = resp
                .json()
                .await
                .map_err(|e| anyhow!("Telegram {method}: invalid response: {}", e.without_url()))?;
            if body.ok {
                return body
                    .result
                    .ok_or_else(|| anyhow!("Telegram {method}: empty result"));
            }
            if let Some(wait) = body.parameters.and_then(|p| p.retry_after) {
                tokio::time::sleep(Duration::from_secs(wait + 1)).await;
                continue;
            }
            bail!(
                "Telegram {method}: {}",
                body.description.unwrap_or_else(|| "unknown error".into())
            );
        }
        bail!("Telegram {method}: rate limited too many times")
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_text() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
    }

    #[test]
    fn picks_media_kind() {
        assert!(matches!(
            UploadKind::for_media("image/png", 100),
            UploadKind::Photo
        ));
        assert!(matches!(
            UploadKind::for_media("image/png", MAX_PHOTO + 1),
            UploadKind::Document
        ));
        assert!(matches!(
            UploadKind::for_media("video/mp4", 1),
            UploadKind::Video
        ));
        assert!(matches!(
            UploadKind::for_media("application/zip", 1),
            UploadKind::Document
        ));
    }

    /// Serves one HTTP request with a successful Bot API reply and returns the raw request.
    async fn mock_server() -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let body = r#"{"ok":true,"result":{"message_id":1}}"#;
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            stream.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (base, handle)
    }

    fn client(base: &str) -> Telegram {
        let cfg = TelegramConfig {
            bot_token: "42:secret".into(),
            api_base: base.into(),
            max_upload_mb: 50,
        };
        Telegram::new(reqwest::Client::builder().no_proxy().build().unwrap(), &cfg).unwrap()
    }

    #[tokio::test]
    async fn uploads_photo_as_multipart() {
        let (base, server) = mock_server().await;
        client(&base)
            .send_file(
                "-100123",
                "cat.png",
                "image/png",
                b"PNGDATA".to_vec(),
                "A cat",
                true,
            )
            .await
            .unwrap();
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /bot42:secret/sendPhoto "));
        assert!(request.contains("name=\"chat_id\"\r\n\r\n-100123"));
        assert!(request.contains("name=\"photo\"; filename=\"cat.png\""));
        assert!(request.contains("name=\"caption\"\r\n\r\nA cat"));
        assert!(request.contains("PNGDATA"));
    }

    #[tokio::test]
    async fn sends_text_message() {
        let (base, server) = mock_server().await;
        client(&base)
            .send_message("@my_channel", "hello https://x")
            .await
            .unwrap();
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /bot42:secret/sendMessage "));
        assert!(request.contains("chat_id=%40my_channel"));
    }

    #[test]
    fn chat_display_name() {
        let chat: Chat = serde_json::from_str(
            r#"{"id":1,"type":"private","first_name":"Ada","username":"ada"}"#,
        )
        .unwrap();
        assert_eq!(chat.display_name(), "Ada (@ada)");
    }
}

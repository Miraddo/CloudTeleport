//! The main window.

use std::collections::HashSet;
use std::sync::Arc;

use egui::{Color32, RichText};

use crate::autostart;
use crate::caption::{self, PLACEHOLDERS};
use crate::config::{AppConfig, Delivery, Route};
use crate::core::{poll, Command, Core, Level, ManualSend, Task};
use crate::google::{self, DriveFile, SHARED_WITH_ME};
use crate::telegram::Chat;
use crate::tray::Tray;

const ACCENT: Color32 = Color32::from_rgb(42, 171, 238);
const OK: Color32 = Color32::from_rgb(76, 175, 80);
const ERR: Color32 = Color32::from_rgb(229, 83, 75);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Routes,
    Share,
    Settings,
}

pub struct App {
    core: Arc<Core>,
    _tray: Tray,
    logo: egui::TextureHandle,
    page: Page,

    // Settings page.
    draft: AppConfig,
    sign_in: Option<Task<String>>,
    bot_check: Option<Task<String>>,
    bot_status: Option<Result<String, String>>,

    // Chats discovered with getUpdates, shared by all chat pickers.
    chats_task: Option<Task<Vec<Chat>>>,
    chats: Vec<Chat>,
    chats_error: Option<String>,

    // Routes page.
    editor: Option<RouteEditor>,
    folder_lookup: Option<Task<(String, String)>>,

    // Share page.
    share_browser: Option<DriveBrowser>,
    share_chat: String,
    share_delivery: Delivery,
    share_caption: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, core: Arc<Core>, tray: Tray) -> Self {
        cc.egui_ctx.style_mut_of(egui::Theme::Dark, style);
        cc.egui_ctx.style_mut_of(egui::Theme::Light, style);
        let draft = core.config.read().clone();
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], &crate::icon::rgba(64)),
            egui::TextureOptions::LINEAR,
        );
        Self {
            core,
            _tray: tray,
            logo,
            page: Page::Dashboard,
            draft,
            sign_in: None,
            bot_check: None,
            bot_status: None,
            chats_task: None,
            chats: Vec::new(),
            chats_error: None,
            editor: None,
            folder_lookup: None,
            share_browser: None,
            share_chat: String::new(),
            share_delivery: Delivery::Document,
            share_caption: "{name}".into(),
        }
    }

    fn poll_tasks(&mut self) {
        if let Some(result) = poll(&mut self.sign_in) {
            match result {
                Ok(account) => self
                    .core
                    .log(Level::Success, format!("Signed in to Google as {account}")),
                Err(e) => self
                    .core
                    .log(Level::Error, format!("Google sign-in failed: {e:#}")),
            }
            self.core.send(Command::ConfigChanged);
        }
        if let Some(result) = poll(&mut self.bot_check) {
            self.bot_status = Some(result.map_err(|e| format!("{e:#}")));
        }
        if let Some(result) = poll(&mut self.chats_task) {
            match result {
                Ok(chats) => {
                    self.chats_error = chats.is_empty().then(|| {
                        "No chats found yet. Send a message to the bot, or post in the group/channel \
                         where it was added, then try again."
                            .to_string()
                    });
                    self.chats = chats;
                }
                Err(e) => self.chats_error = Some(format!("{e:#}")),
            }
        }
        if let Some(result) = poll(&mut self.folder_lookup) {
            match result {
                Ok((folder_id, name)) => self.core.update_config(|c| {
                    for r in c.routes.iter_mut().filter(|r| r.folder_id == folder_id) {
                        r.folder_name = name.clone();
                    }
                }),
                Err(e) => self.core.log(
                    Level::Error,
                    format!("Could not look up the Drive folder: {e:#}"),
                ),
            }
        }
    }

    fn detect_chats(&mut self) {
        match self.core.telegram() {
            Ok(tg) => {
                self.chats_error = None;
                self.chats_task = Some(self.core.spawn(async move { tg.recent_chats().await }));
            }
            Err(e) => self.chats_error = Some(format!("{e:#}")),
        }
    }

    // ---------------------------------------------------------------- layout

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let paused = self.core.is_paused();
        let (busy, problem) = {
            let s = self.core.status.lock();
            (s.busy.clone(), s.problem.clone())
        };
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.image((self.logo.id(), egui::vec2(26.0, 26.0)));
            ui.label(RichText::new("CloudTeleport").strong().size(18.0));
            ui.add_space(12.0);
            let (text, color) = if let Some(busy) = &busy {
                (busy.clone(), ACCENT)
            } else if paused {
                ("Paused".to_string(), Color32::GOLD)
            } else if let Some(problem) = &problem {
                (problem.clone(), ERR)
            } else {
                ("Watching for new files".to_string(), OK)
            };
            if busy.is_some() {
                ui.spinner();
            } else {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 5.0, color);
            }
            ui.label(RichText::new(text).color(color));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Hide to tray")
                    .on_hover_text("Keep running in the background")
                    .clicked()
                {
                    self.core.hide_window();
                }
                let label = if paused { "▶ Resume" } else { "⏸ Pause" };
                if ui.button(label).clicked() {
                    self.core.set_paused(!paused);
                }
                if ui
                    .add_enabled(busy.is_none(), egui::Button::new("⟳ Sync now"))
                    .clicked()
                {
                    self.core.send(Command::SyncNow);
                }
            });
        });
    }

    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        for (page, label) in [
            (Page::Dashboard, "🏠  Dashboard"),
            (Page::Routes, "🔀  Routes"),
            (Page::Share, "📤  Share files"),
            (Page::Settings, "⚙  Settings"),
        ] {
            let selected = self.page == page;
            let button = egui::Button::selectable(selected, RichText::new(label).size(15.0))
                .min_size(egui::vec2(ui.available_width(), 32.0));
            if ui.add(button).clicked() {
                self.page = page;
            }
        }
    }

    // ------------------------------------------------------------- dashboard

    fn dashboard(&mut self, ui: &mut egui::Ui) {
        let cfg = self.core.config.read().clone();
        let signed_in = self.core.google.account();
        let status = self.core.status.lock();

        ui.heading("Dashboard");
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            stat(
                ui,
                "Active routes",
                cfg.routes.iter().filter(|r| r.enabled).count().to_string(),
            );
            stat(ui, "Files sent", status.total_sent.to_string());
            stat(
                ui,
                "Last check",
                status
                    .last_sync
                    .map(|t| t.format("%H:%M:%S").to_string())
                    .unwrap_or("—".into()),
            );
            stat(
                ui,
                "Next check",
                if cfg.paused {
                    "paused".into()
                } else {
                    status
                        .next_sync
                        .map(|t| t.format("%H:%M:%S").to_string())
                        .unwrap_or("—".into())
                },
            );
        });

        let steps = [
            (signed_in.is_some(), "Sign in to Google Drive (Settings)"),
            (
                !cfg.telegram.bot_token.trim().is_empty(),
                "Add your Telegram bot token (Settings)",
            ),
            (
                cfg.routes.iter().any(|r| r.enabled),
                "Create a route from a Drive folder to a chat (Routes)",
            ),
        ];
        if steps.iter().any(|(done, _)| !done) {
            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(RichText::new("Getting started").strong());
                for (done, text) in steps {
                    let (mark, color) = if done {
                        ("✔", OK)
                    } else {
                        ("○", ui.visuals().weak_text_color())
                    };
                    ui.label(RichText::new(format!("{mark}  {text}")).color(color));
                }
            });
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Activity").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Clear").clicked() {
                    self.core.status.lock().log.clear();
                }
            });
        });
        let status = status; // keep the lock for rendering the log
        card(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    if status.log.is_empty() {
                        ui.weak("Nothing happened yet.");
                    }
                    for entry in &status.log {
                        let color = match entry.level {
                            Level::Info => ui.visuals().text_color(),
                            Level::Success => OK,
                            Level::Error => ERR,
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.weak(entry.time.format("%m-%d %H:%M:%S").to_string());
                            ui.label(RichText::new(&entry.text).color(color));
                        });
                    }
                });
        });
    }

    // ---------------------------------------------------------------- routes

    fn routes(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Routes");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("➕ Add route").clicked() {
                    self.editor = Some(RouteEditor::new(Route::default(), true));
                }
            });
        });
        ui.label("Each route watches a Google Drive folder and forwards every new file to a Telegram chat, group or channel.");
        ui.add_space(8.0);

        let routes = self.core.config.read().routes.clone();
        if routes.is_empty() {
            ui.weak("No routes yet — click “Add route”.");
        }
        let mut delete = None;
        let mut toggle = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for route in &routes {
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        let mut enabled = route.enabled;
                        if ui
                            .checkbox(&mut enabled, "")
                            .on_hover_text("Enabled")
                            .changed()
                        {
                            toggle = Some((route.id.clone(), enabled));
                        }
                        ui.vertical(|ui| {
                            let title = if route.name.is_empty() {
                                "Unnamed route"
                            } else {
                                &route.name
                            };
                            ui.label(RichText::new(title).strong());
                            ui.label(format!(
                                "📁 {}   »   💬 {}",
                                route.folder_name, route.chat_id
                            ));
                            ui.weak(route.delivery.label());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("🗑").on_hover_text("Delete").clicked() {
                                delete = Some(route.id.clone());
                            }
                            if ui.button("✏ Edit").clicked() {
                                self.editor = Some(RouteEditor::new(route.clone(), false));
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        });
        if let Some((id, enabled)) = toggle {
            self.core.update_config(|c| {
                if let Some(r) = c.routes.iter_mut().find(|r| r.id == id) {
                    r.enabled = enabled;
                }
            });
        }
        if let Some(id) = delete {
            self.core.update_config(|c| c.routes.retain(|r| r.id != id));
        }
    }

    fn route_editor(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let mut open = true;
        let mut action = EditorAction::None;
        egui::Window::new(if editor.is_new {
            "New route"
        } else {
            "Edit route"
        })
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            action = editor.ui(ui, &self.core, &self.chats, self.chats_task.is_some());
        });
        match action {
            EditorAction::None => {}
            EditorAction::DetectChats => self.detect_chats(),
            EditorAction::Cancel => open = false,
            EditorAction::Save => {
                let mut route = editor.route.clone();
                route.folder_id = google::parse_folder_id(&editor.folder_input);
                if route.folder_name.is_empty() || editor.folder_name_stale {
                    route.folder_name = route.folder_id.clone();
                    let (core, id) = (self.core.clone(), route.folder_id.clone());
                    self.folder_lookup = Some(self.core.spawn(async move {
                        let cfg = core.config.read().google.clone();
                        let name = core.google.folder_name(&cfg, &id).await?;
                        Ok((id, name))
                    }));
                }
                route.chat_id = route.chat_id.trim().to_string();
                self.core
                    .update_config(|c| match c.routes.iter_mut().find(|r| r.id == route.id) {
                        Some(existing) => *existing = route,
                        None => c.routes.push(route),
                    });
                self.core.send(Command::SyncNow);
                open = false;
            }
        }
        if !open {
            self.editor = None;
        }
    }

    // ----------------------------------------------------------------- share

    fn share(&mut self, ui: &mut egui::Ui) {
        ui.heading("Share files");
        ui.label("Pick files from Google Drive and send them to a Telegram chat right now.");
        ui.add_space(8.0);

        if self.core.google.account().is_none() {
            ui.colored_label(ERR, "Sign in to Google in Settings first.");
            return;
        }
        let browser = self
            .share_browser
            .get_or_insert_with(|| DriveBrowser::new(&self.core, false));

        let selected: Vec<DriveFile> = browser
            .items
            .iter()
            .filter(|f| browser.selected.contains(&f.id))
            .cloned()
            .collect();
        let folder_name = browser.current_name();

        card(ui, |ui| {
            egui::Grid::new("share_form")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Send to");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.share_chat)
                                .hint_text("@channel or chat id")
                                .desired_width(220.0),
                        );
                        chat_picker(ui, "share_chats", &mut self.share_chat, &self.chats);
                    });
                    ui.end_row();
                    ui.label("As");
                    delivery_picker(ui, "share_delivery", &mut self.share_delivery);
                    ui.end_row();
                    ui.label("Caption");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.share_caption).desired_width(320.0),
                    )
                    .on_hover_text(format!("Placeholders: {PLACEHOLDERS}"));
                    ui.end_row();
                });
            ui.add_space(4.0);
            let can_send = !selected.is_empty() && !self.share_chat.trim().is_empty();
            let label = format!(
                "📤 Send {} file{}",
                selected.len(),
                if selected.len() == 1 { "" } else { "s" }
            );
            if ui
                .add_enabled(can_send, egui::Button::new(RichText::new(label).strong()))
                .clicked()
            {
                self.core.send(Command::Send(ManualSend {
                    chat_id: self.share_chat.trim().to_string(),
                    files: selected,
                    folder_name,
                    delivery: self.share_delivery,
                    caption: self.share_caption.clone(),
                }));
                self.share_browser.as_mut().unwrap().selected.clear();
                self.core.log(
                    Level::Info,
                    "Queued files for sending — see progress in the status bar",
                );
            }
        });
        ui.add_space(8.0);
        if let Some(browser) = &mut self.share_browser {
            card(ui, |ui| {
                browser.ui(ui, &self.core);
            });
        }
    }

    // -------------------------------------------------------------- settings

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(8.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.google_settings(ui);
            ui.add_space(10.0);
            self.telegram_settings(ui);
            ui.add_space(10.0);
            self.general_settings(ui);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let dirty = !same_settings(&self.draft, &self.core.config.read());
                if ui
                    .add_enabled(
                        dirty,
                        egui::Button::new(RichText::new("💾 Save settings").strong()),
                    )
                    .clicked()
                {
                    self.save_settings();
                }
                if dirty && ui.button("Revert").clicked() {
                    self.draft = self.core.config.read().clone();
                }
            });
            ui.add_space(8.0);
            ui.weak(format!(
                "Settings are stored in {}",
                self.core.paths.config.display()
            ));
        });
    }

    fn save_settings(&mut self) {
        let draft = self.draft.clone();
        let autostart_changed = draft.autostart != self.core.config.read().autostart;
        self.core.update_config(|c| {
            c.google = draft.google;
            c.telegram = draft.telegram;
            c.poll_interval_secs = draft.poll_interval_secs;
            c.start_minimized = draft.start_minimized;
            c.autostart = draft.autostart;
            c.close_to_tray = draft.close_to_tray;
        });
        if autostart_changed {
            if let Err(e) = autostart::set_enabled(self.draft.autostart) {
                self.core.log(Level::Error, format!("{e:#}"));
            }
        }
        self.core.log(Level::Info, "Settings saved");
    }

    fn google_settings(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.label(RichText::new("Google Drive").strong().size(16.0));
            egui::CollapsingHeader::new("How to get an OAuth client ID").show(ui, |ui| {
                ui.label(
                    "1. Open console.cloud.google.com, create a project and enable the “Google Drive API”.\n\
                     2. Configure the OAuth consent screen (External, add yourself as a test user).\n\
                     3. Credentials → Create credentials → OAuth client ID → Application type “Desktop app”.\n\
                     4. Paste the client ID and client secret below, save, then click “Sign in”.",
                );
                ui.hyperlink_to("Open Google Cloud Console", "https://console.cloud.google.com/apis/credentials");
            });
            egui::Grid::new("google")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Client ID");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.google.client_id)
                            .desired_width(380.0),
                    );
                    ui.end_row();
                    ui.label("Client secret");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.google.client_secret)
                            .password(true)
                            .desired_width(380.0),
                    );
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.horizontal(|ui| match self.core.google.account() {
                Some(account) => {
                    ui.colored_label(OK, format!("✔ Signed in as {account}"));
                    if ui.button("Sign out").clicked() {
                        let core = self.core.clone();
                        self.core
                            .rt
                            .spawn(async move { core.google.sign_out().await });
                        self.core.log(Level::Info, "Signed out of Google");
                    }
                }
                None if self.sign_in.is_some() => {
                    ui.spinner();
                    ui.label("Complete the sign-in in your web browser…");
                }
                None => {
                    let saved = self.core.config.read().google.clone();
                    let ready = !saved.client_id.trim().is_empty();
                    if ui
                        .add_enabled(ready, egui::Button::new("🔑 Sign in with Google"))
                        .on_disabled_hover_text("Enter and save the client ID first")
                        .clicked()
                    {
                        let core = self.core.clone();
                        self.sign_in = Some(
                            self.core
                                .spawn(async move { core.google.sign_in(&saved).await }),
                        );
                    }
                }
            });
        });
    }

    fn telegram_settings(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.label(RichText::new("Telegram").strong().size(16.0));
            egui::CollapsingHeader::new("How to set up the bot").show(ui, |ui| {
                ui.label(
                    "1. Talk to @BotFather in Telegram, send /newbot and copy the token.\n\
                     2. Channel: add the bot as an administrator allowed to post messages.\n\
                     3. Group: add the bot to the group.\n\
                     4. Personal chat: open the bot and press Start.\n\
                     Then use “Detect chats” to find the chat ids (or use @channelname for public channels).",
                );
                ui.hyperlink_to("Open @BotFather", "https://t.me/BotFather");
            });
            egui::Grid::new("telegram")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Bot token");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.telegram.bot_token)
                            .password(true)
                            .hint_text("123456:ABC-DEF…")
                            .desired_width(380.0),
                    );
                    ui.end_row();
                    ui.label("Bot API URL");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.telegram.api_base)
                            .desired_width(380.0),
                    )
                    .on_hover_text("Change only if you run your own Telegram Bot API server");
                    ui.end_row();
                    ui.label("Max upload size");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.telegram.max_upload_mb)
                            .range(1..=2000)
                            .suffix(" MB"),
                    )
                    .on_hover_text(
                        "Larger files are shared as a Drive link. The public Bot API allows 50 MB.",
                    );
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let saved_token = !self.core.config.read().telegram.bot_token.trim().is_empty();
                if ui
                    .add_enabled(
                        saved_token && self.bot_check.is_none(),
                        egui::Button::new("Test bot"),
                    )
                    .on_disabled_hover_text("Save the bot token first")
                    .clicked()
                {
                    self.bot_status = None;
                    match self.core.telegram() {
                        Ok(tg) => {
                            self.bot_check = Some(self.core.spawn(async move { tg.get_me().await }))
                        }
                        Err(e) => self.bot_status = Some(Err(format!("{e:#}"))),
                    }
                }
                if ui
                    .add_enabled(
                        saved_token && self.chats_task.is_none(),
                        egui::Button::new("Detect chats"),
                    )
                    .clicked()
                {
                    self.detect_chats();
                }
                if self.bot_check.is_some() || self.chats_task.is_some() {
                    ui.spinner();
                }
                match &self.bot_status {
                    Some(Ok(name)) => {
                        ui.colored_label(OK, format!("✔ Connected as {name}"));
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ERR, e);
                    }
                    None => {}
                }
            });
            chats_table(ui, &self.chats, self.chats_error.as_deref());
        });
    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.label(RichText::new("Background").strong().size(16.0));
            ui.horizontal(|ui| {
                ui.label("Check Drive every");
                ui.add(
                    egui::DragValue::new(&mut self.draft.poll_interval_secs)
                        .range(15..=86_400)
                        .suffix(" s"),
                );
            });
            ui.checkbox(
                &mut self.draft.close_to_tray,
                "Closing the window keeps CloudTeleport running in the tray",
            );
            ui.checkbox(&mut self.draft.start_minimized, "Start hidden in the tray");
            ui.checkbox(&mut self.draft.autostart, "Launch at login");
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Closing the window hides it; the app keeps running in the tray.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.core.is_quitting()
            && self.core.config.read().close_to_tray
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        self.poll_tasks();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui);
            ui.add_space(6.0);
        });
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(170.0)
            .show(ui, |ui| self.nav(ui));
        egui::CentralPanel::default().show(ui, |ui| match self.page {
            Page::Dashboard => self.dashboard(ui),
            Page::Routes => self.routes(ui),
            Page::Share => self.share(ui),
            Page::Settings => self.settings(ui),
        });
        self.route_editor(&ctx);

        // Keep the countdown/status fresh while visible.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

// ------------------------------------------------------------ route editor

enum EditorAction {
    None,
    Save,
    Cancel,
    DetectChats,
}

struct RouteEditor {
    route: Route,
    is_new: bool,
    folder_input: String,
    /// The folder was typed/pasted rather than picked, so its name must be looked up.
    folder_name_stale: bool,
    browser: Option<DriveBrowser>,
}

impl RouteEditor {
    fn new(route: Route, is_new: bool) -> Self {
        Self {
            folder_input: route.folder_id.clone(),
            folder_name_stale: false,
            route,
            is_new,
            browser: None,
        }
    }

    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        core: &Arc<Core>,
        chats: &[Chat],
        detecting: bool,
    ) -> EditorAction {
        let mut action = EditorAction::None;
        egui::Grid::new("route_form")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("Name");
                ui.add(
                    egui::TextEdit::singleline(&mut self.route.name)
                        .hint_text("e.g. Photos → Family group"),
                );
                ui.end_row();

                ui.label("Drive folder");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.folder_input)
                                    .hint_text("Folder link or id")
                                    .desired_width(300.0),
                            )
                            .changed()
                        {
                            self.folder_name_stale = true;
                        }
                        let signed_in = core.google.account().is_some();
                        if ui
                            .add_enabled(signed_in, egui::Button::new("📂 Browse…"))
                            .on_disabled_hover_text("Sign in to Google first")
                            .clicked()
                        {
                            self.browser = match self.browser {
                                Some(_) => None,
                                None => Some(DriveBrowser::new(core, true)),
                            };
                        }
                    });
                    if !self.route.folder_name.is_empty() && !self.folder_name_stale {
                        ui.weak(format!("📁 {}", self.route.folder_name));
                    }
                });
                ui.end_row();
            });

        if let Some(browser) = &mut self.browser {
            let picked = card(ui, |ui| {
                ui.set_max_height(260.0);
                browser.ui(ui, core);
                ui.separator();
                let (id, name) = browser.current();
                let usable = id != SHARED_WITH_ME;
                ui.add_enabled(usable, egui::Button::new(format!("✔ Use “{name}”")))
                    .clicked()
                    .then_some((id, name))
            });
            if let Some((id, name)) = picked {
                self.folder_input = id;
                self.route.folder_name = name;
                self.folder_name_stale = false;
                self.browser = None;
            }
        }

        egui::Grid::new("route_form2")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("Telegram chat");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.route.chat_id)
                                .hint_text("@channel, -100… or chat id")
                                .desired_width(220.0),
                        );
                        chat_picker(ui, "route_chats", &mut self.route.chat_id, chats);
                        if ui
                            .add_enabled(!detecting, egui::Button::new("Detect"))
                            .clicked()
                        {
                            action = EditorAction::DetectChats;
                        }
                        if detecting {
                            ui.spinner();
                        }
                    });
                    ui.weak("The bot must be a member (admin in channels) of the chat.");
                });
                ui.end_row();

                ui.label("Send as");
                delivery_picker(ui, "route_delivery", &mut self.route.delivery);
                ui.end_row();

                ui.label("Caption");
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.route.caption)
                            .desired_rows(2)
                            .desired_width(320.0),
                    );
                    ui.weak(format!("Placeholders: {PLACEHOLDERS}"));
                });
                ui.end_row();

                ui.label("");
                ui.checkbox(
                    &mut self.route.include_existing,
                    "Also send files already in the folder",
                )
                .on_hover_text("Otherwise only files added after the route is created are sent");
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut self.route.enabled, "Enabled");
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let valid =
                !self.folder_input.trim().is_empty() && !self.route.chat_id.trim().is_empty();
            if ui
                .add_enabled(
                    valid,
                    egui::Button::new(RichText::new("Save route").strong()),
                )
                .on_disabled_hover_text("Choose a Drive folder and a Telegram chat")
                .clicked()
            {
                action = EditorAction::Save;
            }
            if ui.button("Cancel").clicked() {
                action = EditorAction::Cancel;
            }
        });
        action
    }
}

// ------------------------------------------------------------ drive browser

struct DriveBrowser {
    folders_only: bool,
    /// Path from the root: (id, name).
    stack: Vec<(String, String)>,
    items: Vec<DriveFile>,
    loading: Option<Task<Vec<DriveFile>>>,
    error: Option<String>,
    selected: HashSet<String>,
}

impl DriveBrowser {
    fn new(core: &Arc<Core>, folders_only: bool) -> Self {
        let mut browser = Self {
            folders_only,
            stack: vec![("root".into(), "My Drive".into())],
            items: Vec::new(),
            loading: None,
            error: None,
            selected: HashSet::new(),
        };
        browser.reload(core);
        browser
    }

    fn current(&self) -> (String, String) {
        self.stack.last().cloned().unwrap_or_default()
    }

    fn current_name(&self) -> String {
        self.current().1
    }

    fn reload(&mut self, core: &Arc<Core>) {
        let (id, _) = self.current();
        let folders_only = self.folders_only;
        let task_core = core.clone();
        self.error = None;
        self.selected.clear();
        self.loading = Some(core.spawn(async move {
            let cfg = task_core.config.read().google.clone();
            task_core.google.list_folder(&cfg, &id, folders_only).await
        }));
    }

    fn set_root(&mut self, core: &Arc<Core>, id: &str, name: &str) {
        self.stack = vec![(id.into(), name.into())];
        self.reload(core);
    }

    fn ui(&mut self, ui: &mut egui::Ui, core: &Arc<Core>) {
        if let Some(result) = poll(&mut self.loading) {
            match result {
                Ok(items) => self.items = items,
                Err(e) => {
                    self.items.clear();
                    self.error = Some(format!("{e:#}"));
                }
            }
        }

        ui.horizontal(|ui| {
            let root = self.stack.first().map(|s| s.0.clone()).unwrap_or_default();
            if ui.selectable_label(root == "root", "My Drive").clicked() {
                self.set_root(core, "root", "My Drive");
            }
            if ui
                .selectable_label(root == SHARED_WITH_ME, "Shared with me")
                .clicked()
            {
                self.set_root(core, SHARED_WITH_ME, "Shared with me");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                    self.reload(core);
                }
            });
        });

        // Breadcrumb.
        let mut go_to = None;
        ui.horizontal_wrapped(|ui| {
            for (i, (_, name)) in self.stack.iter().enumerate() {
                if i > 0 {
                    ui.weak("»");
                }
                if ui.link(name).clicked() {
                    go_to = Some(i);
                }
            }
        });
        if let Some(i) = go_to {
            if i + 1 < self.stack.len() {
                self.stack.truncate(i + 1);
                self.reload(core);
            }
        }
        ui.separator();

        if self.loading.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading…");
            });
            return;
        }
        if let Some(error) = &self.error {
            ui.colored_label(ERR, error);
            return;
        }

        let files: Vec<&DriveFile> = self.items.iter().filter(|f| !f.is_folder()).collect();
        if !self.folders_only && !files.is_empty() {
            ui.horizontal(|ui| {
                if ui.small_button("Select all").clicked() {
                    self.selected = files.iter().map(|f| f.id.clone()).collect();
                }
                if ui.small_button("Select none").clicked() {
                    self.selected.clear();
                }
                ui.weak(format!("{} selected", self.selected.len()));
            });
        }

        let mut open = None;
        egui::ScrollArea::vertical()
            .id_salt("drive_items")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if self.items.is_empty() {
                    ui.weak(if self.folders_only {
                        "No sub-folders here."
                    } else {
                        "This folder is empty."
                    });
                }
                for item in &self.items {
                    if item.is_folder() {
                        if ui.link(format!("📁 {}", item.name)).clicked() {
                            open = Some((item.id.clone(), item.name.clone()));
                        }
                    } else if !self.folders_only {
                        ui.horizontal(|ui| {
                            let mut checked = self.selected.contains(&item.id);
                            if ui
                                .checkbox(
                                    &mut checked,
                                    format!("{} {}", file_emoji(&item.mime_type), item.name),
                                )
                                .changed()
                            {
                                if checked {
                                    self.selected.insert(item.id.clone());
                                } else {
                                    self.selected.remove(&item.id);
                                }
                            }
                            let size = item.size.map(caption::human_size).unwrap_or_default();
                            ui.weak(size);
                        });
                    }
                }
            });
        if let Some(folder) = open {
            self.stack.push(folder);
            self.reload(core);
        }
    }
}

// ------------------------------------------------------------------ helpers

fn style(style: &mut egui::Style) {
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.visuals.selection.bg_fill = ACCENT.linear_multiply(0.6);
    style.visuals.hyperlink_color = ACCENT;
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::group(ui.style())
        .inner_margin(12.0)
        .corner_radius(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn stat(ui: &mut egui::Ui, label: &str, value: String) {
    egui::Frame::group(ui.style())
        .inner_margin(12.0)
        .corner_radius(8.0)
        .show(ui, |ui| {
            ui.set_min_width(150.0);
            ui.vertical(|ui| {
                ui.weak(label);
                ui.label(RichText::new(value).size(20.0).strong());
            });
        });
}

fn delivery_picker(ui: &mut egui::Ui, id: &str, delivery: &mut Delivery) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(delivery.label())
        .width(260.0)
        .show_ui(ui, |ui| {
            for option in Delivery::ALL {
                ui.selectable_value(delivery, option, option.label());
            }
        });
}

fn chat_picker(ui: &mut egui::Ui, id: &str, chat_id: &mut String, chats: &[Chat]) {
    if chats.is_empty() {
        return;
    }
    egui::ComboBox::from_id_salt(id)
        .selected_text("Pick…")
        .width(80.0)
        .show_ui(ui, |ui| {
            for chat in chats {
                let label = format!("{} ({})", chat.display_name(), chat.kind);
                if ui
                    .selectable_label(*chat_id == chat.id.to_string(), label)
                    .clicked()
                {
                    *chat_id = chat.id.to_string();
                }
            }
        });
}

fn chats_table(ui: &mut egui::Ui, chats: &[Chat], error: Option<&str>) {
    if let Some(error) = error {
        ui.colored_label(ERR, error);
    }
    if chats.is_empty() {
        return;
    }
    ui.add_space(6.0);
    egui::Grid::new("chats")
        .striped(true)
        .num_columns(3)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            ui.strong("Chat");
            ui.strong("Type");
            ui.strong("Chat id");
            ui.end_row();
            for chat in chats {
                ui.label(chat.display_name());
                ui.label(&chat.kind);
                ui.horizontal(|ui| {
                    ui.monospace(chat.id.to_string());
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(chat.id.to_string());
                    }
                });
                ui.end_row();
            }
        });
}

fn file_emoji(mime: &str) -> &'static str {
    match mime.split('/').next().unwrap_or("") {
        "image" => "🖼",
        "video" => "🎞",
        "audio" => "🎵",
        _ if mime.starts_with("application/vnd.google-apps") => "📝",
        _ => "📄",
    }
}

/// Compares the parts of the configuration edited on the Settings page.
fn same_settings(a: &AppConfig, b: &AppConfig) -> bool {
    a.google.client_id == b.google.client_id
        && a.google.client_secret == b.google.client_secret
        && a.telegram.bot_token == b.telegram.bot_token
        && a.telegram.api_base == b.telegram.api_base
        && a.telegram.max_upload_mb == b.telegram.max_upload_mb
        && a.poll_interval_secs == b.poll_interval_secs
        && a.start_minimized == b.start_minimized
        && a.autostart == b.autostart
        && a.close_to_tray == b.close_to_tray
}

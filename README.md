# CloudTeleport

A small desktop app, written in Rust, that forwards files from **Google Drive** to **Telegram**: a channel, a group, or a personal chat.

- **Routes**: watch a Drive folder and post every new file to a Telegram chat automatically.
- **Share files**: browse your Drive and send selected files to any chat right away.
- **Runs in the background**: closing the window keeps it running with an icon in the system tray (Windows), the menu bar (macOS) or the panel indicator area (Linux). Launch at login is optional.
- Uploads files as documents, or as native photos, videos and audio. It can also post just the Drive link.
- Google Docs, Sheets, Slides and Drawings are exported to PDF, XLSX or PNG.
- Files over the Telegram upload limit (50 MB on the public Bot API) are shared as Drive links. The limit can be raised if you run a [local Bot API server](https://github.com/tdlib/telegram-bot-api).
- Captions use templates: `{name} {link} {folder} {size} {type} {created} {modified}`.

Built with [egui/eframe](https://github.com/emilk/egui) for the UI, [tray-icon](https://github.com/tauri-apps/tray-icon) for the tray and [tokio](https://tokio.rs) for the background worker.

## Download

Prebuilt packages are attached to each [GitHub release](https://github.com/Miraddo/CloudTeleport/releases):

| Platform | File |
|---|---|
| Windows (x86_64) | `CloudTeleport-<version>-windows-x86_64.exe` (portable) or `.zip` |
| macOS (Apple Silicon + Intel) | `CloudTeleport-<version>-macos-universal.dmg` |
| Linux (x86_64, aarch64) | `cloudteleport_<version>_<arch>.deb` or `.tar.gz` |

The macOS app is signed ad-hoc but not notarized. The first time you open it, right-click the app and choose **Open**, or run `xattr -dr com.apple.quarantine /Applications/CloudTeleport.app`. Windows SmartScreen may also warn about an unsigned app; click **More info → Run anyway**.

## Building

Install Rust 1.95 or newer from <https://rustup.rs>.

**Linux** also needs GTK and AppIndicator headers:

```sh
# Debian / Ubuntu
sudo apt install libgtk-3-dev libxdo-dev libayatana-appindicator3-dev
# Fedora
sudo dnf install gtk3-devel libxdo-devel libayatana-appindicator-gtk3-devel
# Arch
sudo pacman -S gtk3 xdotool libayatana-appindicator
```

Then:

```sh
cargo run --release
```

On GNOME, the tray icon only appears if the *AppIndicator and KStatusNotifierItem Support* extension is installed.


## Setup

### 1. Google Drive

CloudTeleport reads your Drive through your own Google OAuth client, so your data never passes through a third party.

1. Open the [Google Cloud Console](https://console.cloud.google.com/), create a project and enable the **Google Drive API**.
2. Configure the **OAuth consent screen**: choose External and add your Google account as a test user.
3. Go to **Credentials → Create credentials → OAuth client ID** and choose the application type **Desktop app**.
4. In CloudTeleport, open **Settings**, paste the client ID and client secret, click **Save settings**, then click **Sign in with Google**.

The app requests the read-only scope `drive.readonly`.

### 2. Telegram bot

1. Message [@BotFather](https://t.me/BotFather), send `/newbot` and copy the token.
2. Give the bot access to the destination:
   - **Channel**: add the bot as an administrator that can post messages.
   - **Group**: add the bot to the group.
   - **Personal chat**: open the bot and press **Start**.
3. Paste the token in **Settings**, save, then click **Test bot**.
4. Click **Detect chats** to list the chats the bot has seen, with their ids. For a public channel you can also use `@channelname`.

### 3. Routes

Go to **Routes → Add route**:

- **Drive folder**: browse to it, or paste a folder link or id.
- **Telegram chat**: pick a detected chat or type its id.
- **Send as**: upload as a file, as a photo/video/audio, or post the link only.
- **Also send files already in the folder**: when this is off, only files added after you create the route are sent.

The folder is checked at the interval you set in Settings (60 seconds by default). **Sync now** in the window or the tray menu checks immediately.

## Tray menu

| Item | Action |
|---|---|
| Open CloudTeleport | Show the window (left-clicking the icon also works on Windows and macOS) |
| Sync now | Check all routes now |
| Pause / resume syncing | Toggle automatic forwarding |
| Quit | Exit the app |

## Where data is stored

Everything is stored in your OS config directory: `~/.config/cloudteleport` on Linux, `~/Library/Application Support/dev.CloudTeleport.CloudTeleport` on macOS, and `%APPDATA%\CloudTeleport\CloudTeleport\config` on Windows. The files are:

- `config.json`: settings, bot token and routes
- `google_tokens.json`: Google OAuth tokens
- `state.json`: which files each route has already handled

On Unix, these files are created readable only by your user.

## How syncing works

On each check, every enabled route lists its folder. Files it hasn't handled before are sent, oldest first. A file that fails 3 times in a row is skipped and the error is logged on the Dashboard. Each file id is recorded as soon as it's sent, so restarting the app doesn't send duplicates.

## CI/CD

- **CI** (`.github/workflows/ci.yml`) runs on every push and pull request. It checks formatting, then runs clippy and the tests on Linux, Windows and macOS.
- **Release** (`.github/workflows/release.yml`) builds the packages listed under *Download* for every platform.
  - Push a version tag to publish a GitHub release. The tag must match the `version` in `Cargo.toml`:
    ```sh
    git tag v0.1.0
    git push origin v0.1.0
    ```
  - Or run the workflow manually from the Actions tab. The packages are then available as workflow artifacts instead of a release.

## Development

```sh
cargo test
cargo clippy --all-targets
RUST_LOG=cloudteleport=debug cargo run
```

| Module | Purpose |
|---|---|
| `main.rs` | Starts the tokio runtime, the worker, the window and the tray |
| `ui.rs` | egui window: dashboard, routes, share, settings |
| `tray.rs` | Tray icon and menu (runs a GTK loop on its own thread on Linux) |
| `worker.rs` | Background polling and delivery loop |
| `google.rs` | OAuth loopback flow with PKCE, and the Drive v3 client |
| `telegram.rs` | Bot API client |
| `config.rs` | Settings, tokens and sync state persistence |

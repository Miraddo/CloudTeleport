//! "Launch at login" support.

use anyhow::{Context, Result};
use auto_launch::AutoLaunchBuilder;

/// Command-line flag used by the login item to start hidden in the tray.
pub const MINIMIZED_FLAG: &str = "--minimized";

pub fn set_enabled(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe().context("locating the executable")?;
    let launcher = AutoLaunchBuilder::new()
        .set_app_name("CloudTeleport")
        .set_app_path(&exe.to_string_lossy())
        .set_args(&[MINIMIZED_FLAG])
        .build()
        .context("configuring launch at login")?;
    let active = launcher.is_enabled().unwrap_or(false);
    if enabled && !active {
        launcher.enable().context("enabling launch at login")?;
    } else if !enabled && active {
        launcher.disable().context("disabling launch at login")?;
    }
    Ok(())
}

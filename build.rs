fn main() {
    // Embed the icon and version info into the Windows executable.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("ProductName", "CloudTeleport")
            .set(
                "FileDescription",
                "CloudTeleport — Google Drive to Telegram",
            );
        res.compile().expect("failed to embed Windows resources");
    }
}

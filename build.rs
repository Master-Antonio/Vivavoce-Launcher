fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("ui/assets/icon.ico");
        res.set("ProductName", "VivaVoce Voiceover Launcher");
        res.set("FileDescription", "VivaVoce Launcher — WoW Forever");
        res.set("LegalCopyright", "Copyright © 2026 VivaVoce Team");
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        res.set("FileVersion", env!("CARGO_PKG_VERSION"));
        if let Err(e) = res.compile() {
            eprintln!("Warning: Failed to compile Windows resource: {}", e);
        }
    }

    slint_build::compile("ui/launcher.slint").expect("Failed to compile Slint UI definition");
}

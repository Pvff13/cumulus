fn main() {
    // Embed the app icon in the .exe so Explorer, Start-menu shortcuts and the
    // taskbar show it (a window icon set at runtime isn't enough for those).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/cumulus.rc");
        println!("cargo:rerun-if-changed=assets/icon.ico");
        embed_resource::compile("assets/cumulus.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("compile Windows resources");
    }
}

use std::time::{SystemTime, UNIX_EPOCH};

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

    // Build number for self-updates: when the code changes, this script runs
    // again and stamps a newer number (seconds since 1970).
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    println!("cargo:rustc-env=CUMULUS_BUILD_ID={stamp}");

    // Where new builds of this project appear; installed copies update from
    // here. Public release builds (CUMULUS_PUBLIC_BUILD=1) leave this out so
    // they don't carry the build machine's folder path.
    println!("cargo:rerun-if-env-changed=CUMULUS_PUBLIC_BUILD");
    if std::env::var_os("CUMULUS_PUBLIC_BUILD").is_none() {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let source = std::path::Path::new(&manifest).join("target").join("release").join("cumulus.exe");
        println!("cargo:rustc-env=CUMULUS_UPDATE_SOURCE={}", source.display());
    }
}

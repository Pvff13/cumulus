// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod auth;
mod cache;
mod config;
mod paths;
mod player;
mod ui;

fn main() {
    // The sign-in window runs as a separate process (see auth::login) so the
    // WebView2 engine is fully torn down once the token has been captured.
    if std::env::args().any(|a| a == auth::login::LOGIN_ARG) {
        std::process::exit(auth::login::run());
    }
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::args().skip_while(|a| a != "--probe").nth(1) {
        return dev_probe(&path);
    }
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::args().skip_while(|a| a != "--dev-icon").nth(1) {
        return ui::write_icon_files(std::path::Path::new(&dir));
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Cumulus")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([720.0, 480.0])
            .with_icon(std::sync::Arc::new(ui::window_icon())),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "Cumulus",
        options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc)))),
    ) {
        eprintln!("cumulus: {e}");
        std::process::exit(1);
    }
}

/// Debug builds only: `cumulus --probe /me` calls the API with the saved token
/// and prints the response's structure (keys, types, list lengths) and a few
/// short sample values, so endpoints can be mapped without exposing the token.
#[cfg(debug_assertions)]
fn dev_probe(path: &str) {
    let api = api::Api::new(config::Config::load().client_id);
    api.set_token(auth::load_token().ok().flatten());
    let path = path.replace("{me}", &api.me().map(|u| u.id.to_string()).unwrap_or_default());
    match api.get::<serde_json::Value>(&path) {
        Ok(v) if std::env::var_os("CUMULUS_PROBE_RAW").is_some() => println!("{v}"),
        Ok(v) => outline(&v, "", 0),
        Err(e) => println!("error: {e}"),
    }
}

#[cfg(debug_assertions)]
fn outline(v: &serde_json::Value, key: &str, depth: usize) {
    use serde_json::Value;
    let pad = "  ".repeat(depth);
    match v {
        Value::Object(m) if depth < 4 => {
            println!("{pad}{key}{{}}");
            for (k, v) in m {
                outline(v, k, depth + 1);
            }
        }
        Value::Array(a) => {
            println!("{pad}{key}[{}]", a.len());
            if let Some(first) = a.first().filter(|_| depth < 4) {
                outline(first, "[0]", depth + 1);
            }
        }
        Value::String(s) => println!("{pad}{key}: \"{}\"", s.chars().take(60).collect::<String>()),
        other => println!("{pad}{key}: {}", other.to_string().chars().take(60).collect::<String>()),
    }
}

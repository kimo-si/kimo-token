// Prevents an extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    eprintln!("kimo-token v{} build {} starting", env!("CARGO_PKG_VERSION"), env!("KIMO_TOKEN_BUILD_ID"));
    kimo_token::run()
}

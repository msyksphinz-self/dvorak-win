#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod engine;
mod layout;
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    win::app::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("dvorak-win は Windows 専用です (このビルドはテスト用)");
}

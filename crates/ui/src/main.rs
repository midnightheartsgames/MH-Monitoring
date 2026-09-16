//! MH Monitor — оверлей производительности (PLAN.md §6/P4).
//!
//! Пока движок работает в этом же процессе (разделение на службу — P6), поэтому для кадров и
//! температуры CPU приложение нужно запускать от администратора. Без прав HUD так и скажет.

// В отладочной сборке консоль остаётся: в неё пишут паники.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg_attr(not(windows), allow(dead_code))]
mod format;
#[cfg_attr(not(windows), allow(dead_code))]
mod settings;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod controls;
#[cfg(windows)]
mod diag;
#[cfg(windows)]
mod hud;
#[cfg(windows)]
mod settings_window;
#[cfg(windows)]
mod theme;

#[cfg(windows)]
fn main() -> eframe::Result {
    app::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("MH Monitor работает только под Windows");
}

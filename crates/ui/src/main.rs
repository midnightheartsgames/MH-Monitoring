//! MH Monitoring — оверлей производительности (PLAN.md §6/P4).
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
mod backend;
#[cfg(windows)]
mod controls;
#[cfg(windows)]
mod diag;
#[cfg(windows)]
mod hud;
#[cfg(windows)]
mod installer;
#[cfg(windows)]
mod remote;
#[cfg(windows)]
mod server;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod settings_window;
#[cfg(windows)]
mod theme;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    // Режимы без окна: служба, отладочный сервер, установка. Остальное — оверлей.
    match std::env::args().nth(1).as_deref() {
        Some("--service") => match service::run_dispatcher() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(1),
        },
        Some("--serve") => match service::run_in_console() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("сервер: {error}");
                ExitCode::from(1)
            }
        },
        Some("--install") => report(installer::install()),
        Some("--uninstall") => {
            let arguments: Vec<String> = std::env::args().collect();
            match installer::uninstall(&arguments) {
                Ok(installer::Uninstall::Cancelled) => ExitCode::from(installer::EXIT_CANCELLED),
                Ok(installer::Uninstall::RemovedData) => {
                    report(Ok(()));
                    ExitCode::from(installer::EXIT_DATA_REMOVED)
                }
                other => report(other.map(|_| ())),
            }
        }
        Some("--install-service") => report(service::install()),
        Some("--start-service") => report(service::start()),
        Some("--uninstall-service") => report(service::uninstall()),
        _ => match app::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(1)
            }
        },
    }
}

/// Итог установки: код возврата читает UI, текст — журнал.
#[cfg(windows)]
fn report(result: Result<(), String>) -> std::process::ExitCode {
    diag::start(settings::local_dir().join("service-setup.log"));
    match result {
        Ok(()) => {
            diag::log("готово");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            diag::log(format!("ошибка: {error}"));
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("MH Monitoring работает только под Windows");
}

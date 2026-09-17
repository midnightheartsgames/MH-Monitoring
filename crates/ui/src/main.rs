//! MH Monitoring — оверлей производительности (PLAN.md §6/P4).
//!
//! Кадры и температуру CPU меряет служба `MH-Monitoring-Service.exe` (P6, P9). Без неё движок
//! работает в этом процессе и видит всё только с правами администратора.

// В отладочной сборке консоль остаётся: в неё пишут паники.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg_attr(not(windows), allow(dead_code))]
mod blocks;
#[cfg_attr(not(windows), allow(dead_code))]
mod format;
#[cfg_attr(not(windows), allow(dead_code))]
mod games;
#[cfg_attr(not(windows), allow(dead_code))]
mod placement;
#[cfg_attr(not(windows), allow(dead_code))]
mod settings;
#[cfg_attr(not(windows), allow(dead_code))]
mod units;

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
mod preview;
#[cfg(windows)]
mod remote;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod settings_window;
#[cfg(windows)]
mod setup_window;
#[cfg(windows)]
mod strip;
#[cfg(windows)]
mod theme;
#[cfg(windows)]
mod widgets;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    // Режимы без окна: установка и служба. Остальное — оверлей.
    match std::env::args().nth(1).as_deref() {
        // Регистрация службы из версий до P9 запускает этот файл. Оверлей от SYSTEM в сеансе 0
        // запускать нельзя: отказ, и SCM видит сбой, пока программу не переустановят.
        Some("--service" | "--serve") => {
            eprintln!("служба теперь — {}; переустановите MH Monitoring", mh_ipc::SERVICE_EXE_NAME);
            ExitCode::from(1)
        }
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
        // Ярлык игры: запустить её, а дальше — обычный оверлей (если он уже работает, эта копия
        // тут же выйдет).
        Some(games::LAUNCH_ARG) => {
            games::launch_from_arguments(&std::env::args().collect::<Vec<_>>());
            run_overlay()
        }
        _ => run_overlay(),
    }
}

#[cfg(windows)]
fn run_overlay() -> std::process::ExitCode {
    match app::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(1)
        }
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

//! Служба MH Monitoring (PLAN.md §6/P6, §6/P9).
//!
//! `MH-Monitoring-Service.exe --service` запускает SCM; `--serve` — тот же сервер в консоли, для
//! отладки. Устанавливает и запускает службу `MH-Monitoring.exe`.

#[cfg(windows)]
mod server;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

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
        _ => {
            eprintln!(
                "Это служба MH Monitoring. Её ставит и запускает MH-Monitoring.exe; \
                 для отладки — `--serve`."
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("MH Monitoring работает только под Windows");
}

/// `%LOCALAPPDATA%\MH Monitoring`. У службы это профиль SYSTEM, куда обычный пользователь не пишет.
#[cfg(windows)]
fn local_dir() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("MH Monitoring")
}

#[cfg(windows)]
mod service {
    use std::ffi::OsString;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use mh_ipc::SERVICE_NAME;
    use mh_platform::diag;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    use crate::server;

    define_windows_service!(ffi_service_main, service_main);

    /// Точка входа `--service`. Возвращается, когда SCM остановил службу.
    pub fn run_dispatcher() -> windows_service::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    }

    fn service_main(_arguments: Vec<OsString>) {
        diag::start(crate::local_dir().join("service.log"));
        diag::log(format!("служба MH Monitoring {}", env!("CARGO_PKG_VERSION")));
        if let Err(error) = run_service() {
            diag::log(format!("служба завершилась с ошибкой: {error}"));
        }
    }

    fn run_service() -> windows_service::Result<()> {
        let stop = Arc::new(AtomicBool::new(false));
        let handler_stop = Arc::clone(&stop);
        let status =
            service_control_handler::register(SERVICE_NAME, move |control| match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    handler_stop.store(true, Ordering::SeqCst);
                    server::wake();
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            })?;

        let report = |state: ServiceState, accept: ServiceControlAccept, code: u32| {
            status.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: accept,
                exit_code: ServiceExitCode::Win32(code),
                checkpoint: 0,
                wait_hint: Duration::from_secs(5),
                process_id: None,
            })
        };

        report(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            0,
        )?;
        let code = match server::run(stop) {
            Ok(()) => 0,
            Err(error) => {
                diag::log(format!("сервер: {error}"));
                error.raw_os_error().unwrap_or(1) as u32
            }
        };
        report(ServiceState::Stopped, ServiceControlAccept::empty(), code)?;
        Ok(())
    }

    /// Отладочный режим `--serve`: тот же сервер в обычном процессе, до Ctrl+C.
    pub fn run_in_console() -> std::io::Result<()> {
        diag::start(crate::local_dir().join("serve.log"));
        diag::log("сервер в консоли");
        let stop = Arc::new(AtomicBool::new(false));
        server::run(stop)
    }
}

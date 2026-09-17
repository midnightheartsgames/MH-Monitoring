//! Режим службы Windows и её установка (PLAN.md §6/P6).
//!
//! Один исполняемый файл: `mh-monitor --service` запускает SCM, `--install-service` и
//! `--uninstall-service` вызывает UI с запросом UAC.

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::{diag, server, settings};

pub const SERVICE_NAME: &str = "MHMonitor";
const DISPLAY_NAME: &str = "MH Monitor";
const DESCRIPTION: &str =
    "Захват кадров (ETW, PresentMon) и датчиков для оверлея MH Monitor без прав администратора.";

define_windows_service!(ffi_service_main, service_main);

/// Точка входа `--service`. Возвращается, когда SCM остановил службу.
pub fn run_dispatcher() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_arguments: Vec<OsString>) {
    diag::start(settings::local_dir().join("service.log"));
    diag::log(format!("служба MH Monitor {}", env!("CARGO_PKG_VERSION")));
    if let Err(error) = run_service() {
        diag::log(format!("служба завершилась с ошибкой: {error}"));
    }
}

fn run_service() -> windows_service::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let handler_stop = Arc::clone(&stop);
    let status = service_control_handler::register(SERVICE_NAME, move |control| match control {
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

    report(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, 0)?;
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
    diag::start(settings::local_dir().join("serve.log"));
    diag::log("сервер в консоли");
    let stop = Arc::new(AtomicBool::new(false));
    server::run(stop)
}

/// `--install-service`: ставит службу на этот исполняемый файл и запускает её.
///
/// Служба работает от SYSTEM и запускает **этот** exe: держать его нужно там, куда обычный
/// пользователь писать не может (Program Files), иначе подмена файла даёт права SYSTEM.
pub fn install() -> Result<(), String> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(describe)?;
    // Старая регистрация — снимаем: путь к exe мог смениться.
    if let Ok(existing) = manager.open_service(SERVICE_NAME, removal_access()) {
        stop_and_delete(&existing)?;
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: executable,
        launch_arguments: vec![OsString::from("--service")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = manager
        .create_service(&info, ServiceAccess::START | ServiceAccess::CHANGE_CONFIG)
        .map_err(describe)?;
    let _ = service.set_description(DESCRIPTION);
    service.start::<&str>(&[]).map_err(describe)?;
    Ok(())
}

/// `--uninstall-service`: останавливает и удаляет службу.
pub fn uninstall() -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(describe)?;
    let service = manager.open_service(SERVICE_NAME, removal_access()).map_err(describe)?;
    stop_and_delete(&service)
}

/// Установлена ли служба — без прав администратора.
pub fn is_installed() -> bool {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|manager| manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS))
        .is_ok()
}

fn removal_access() -> ServiceAccess {
    ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS
}

fn stop_and_delete(service: &windows_service::service::Service) -> Result<(), String> {
    if service.query_status().map_err(describe)?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        for _ in 0..50 {
            match service.query_status() {
                Ok(status) if status.current_state == ServiceState::Stopped => break,
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
    service.delete().map_err(describe)
}

fn describe(error: windows_service::Error) -> String {
    match &error {
        windows_service::Error::Winapi(io) => io.to_string(),
        other => other.to_string(),
    }
}

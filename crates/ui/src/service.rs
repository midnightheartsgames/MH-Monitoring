//! Установка и управление службой Windows (PLAN.md §6/P6, §6/P9).
//!
//! Сама служба — отдельный `MH-Monitoring-Service.exe` (крейт `mh-service`). Здесь только клиент
//! SCM: поставить, запустить, проверить, снять. `--install-service`, `--start-service` и
//! `--uninstall-service` вызывает UI с запросом UAC.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mh_ipc::SERVICE_NAME;
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl, ServiceFailureActions,
    ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use crate::diag;

const DISPLAY_NAME: &str = "MH Monitoring";
const DESCRIPTION: &str =
    "Захват кадров (ETW, PresentMon) и датчиков для оверлея MH Monitoring без прав администратора.";

/// `--install-service`: ставит службу на `MH-Monitoring-Service.exe` рядом с этим файлом — для
/// разработки. Обычная установка (`--install`) ставит её на копию в Program Files.
pub fn install() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let service = executable.with_file_name(mh_ipc::SERVICE_EXE_NAME);
    if !service.is_file() {
        return Err(format!("нет файла службы: {}", service.display()));
    }
    install_at(&service)
}

/// Ставит службу на `executable` (`MH-Monitoring-Service.exe`) и запускает её.
///
/// Служба работает от SYSTEM и запускает **этот** exe: держать его нужно там, куда обычный
/// пользователь писать не может (Program Files), иначе подмена файла даёт права SYSTEM.
pub fn install_at(executable: &Path) -> Result<(), String> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(describe)?;
    // Старая регистрация — снимаем: путь к exe мог смениться.
    if let Ok(existing) = manager.open_service(SERVICE_NAME, removal_access()) {
        stop_and_delete(&existing)?;
    }
    let executable = executable.to_path_buf();
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
    if let Err(error) = service.update_failure_actions(restart_on_failure()) {
        diag::log(format!("перезапуск службы после сбоя не настроен: {error}"));
    }
    service.start::<&str>(&[]).map_err(describe)?;
    Ok(())
}

/// Служба, упавшая или завершённая извне, поднимается снова. В диспетчере задач оверлей и служба
/// называются одинаково, и завершить службу вместе с оверлеем легко: без этого оверлей остался бы
/// без кадров до перезагрузки.
fn restart_on_failure() -> ServiceFailureActions {
    let restart =
        ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(2) };
    ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    }
}

/// Установлена ли служба и работает ли она. Прав не требует.
pub fn is_running() -> bool {
    state() == Some(ServiceState::Running)
}

/// Установлена, но стоит — её можно запустить. «Запускается» сюда не входит: при входе в систему
/// служба с автозапуском часто ещё поднимается, и UAC в этот момент был бы лишним.
pub fn is_stopped() -> bool {
    state() == Some(ServiceState::Stopped)
}

/// Состояние службы; `None` — не установлена или SCM недоступен.
fn state() -> Option<ServiceState> {
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok()?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS).ok()?;
    service.query_status().ok().map(|status| status.current_state)
}

/// `--start-service`: запускает остановленную службу. Требует прав администратора.
pub fn start() -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(describe)?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::START | ServiceAccess::CHANGE_CONFIG)
        .map_err(describe)?;
    // Установки до появления автоперезапуска получают его здесь.
    let _ = service.update_failure_actions(restart_on_failure());
    service.start::<&str>(&[]).map_err(describe)
}

/// `--uninstall-service`: останавливает и удаляет службу.
pub fn uninstall() -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(describe)?;
    let service = manager.open_service(SERVICE_NAME, removal_access()).map_err(describe)?;
    stop_and_delete(&service)
}

/// Останавливает и удаляет службу, если она есть. Её отсутствие — не ошибка.
pub fn remove_if_present() -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(describe)?;
    match manager.open_service(SERVICE_NAME, removal_access()) {
        Ok(service) => stop_and_delete(&service),
        Err(_) => Ok(()),
    }
}

/// Путь к exe, на который зарегистрирована служба.
pub fn registered_executable() -> Option<PathBuf> {
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok()?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_CONFIG).ok()?;
    let command = service.query_config().ok()?.executable_path;
    Some(executable_of_command(&command.to_string_lossy()))
}

/// SCM хранит командную строку целиком: `"C:\путь\MH-Monitoring-Service.exe" --service`.
fn executable_of_command(command: &str) -> PathBuf {
    let command = command.trim();
    let path = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => command.split(" --").next().unwrap_or(command),
    };
    PathBuf::from(path)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_executable_is_cut_out_of_the_command_line() {
        assert_eq!(
            executable_of_command(
                r#""C:\Program Files\MH Monitoring\MH-Monitoring-Service.exe" --service"#
            ),
            PathBuf::from(r"C:\Program Files\MH Monitoring\MH-Monitoring-Service.exe")
        );
        assert_eq!(
            executable_of_command(r"I:\x\MH-Monitoring.exe --service"),
            PathBuf::from(r"I:\x\MH-Monitoring.exe")
        );
    }
}

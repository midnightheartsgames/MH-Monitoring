//! Самоустановка: один exe ставит себя сам (PLAN.md §6/P7).
//!
//! `--install` (с правами администратора): копия в `Program Files\MH Monitoring`, рядом
//! `MH-Monitoring-Service.exe` и служба на него, ярлыки в «Пуске» и на рабочем столе, запись в
//! «Программы и компоненты».
//! `--uninstall` — обратное; перед удалением спрашивает, удалять ли данные пользователя.
//!
//! Служба запускается от SYSTEM, поэтому её exe обязан лежать там, куда обычный пользователь не
//! пишет: иначе подмена файла дала бы права системы (PLAN.md §6/P6).

use std::path::{Path, PathBuf};

use mh_platform::instance::{Answer, ask, is_elevated, message, processes_from, terminate};
use mh_platform::registry::{self, Hive, Value};
use mh_platform::shortcut;

use crate::{diag, service, settings};

const APP_DIR: &str = "MH Monitoring";
const EXE_NAME: &str = "MH-Monitoring.exe";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\MHMonitor";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "MH Monitoring";
const SHORTCUT_NAME: &str = "MH Monitoring.lnk";

/// Следы сборок до переименования: папка `MH Monitor`, файл `mh-monitor.exe`, ярлык и автозапуск
/// со старым именем. Служба называлась так же — её снимает общий путь.
const LEGACY_APP_DIR: &str = "MH Monitor";
const LEGACY_EXE_NAME: &str = "mh-monitor.exe";
const LEGACY_SHORTCUT_NAME: &str = "MH Monitor.lnk";
const LEGACY_RUN_VALUE: &str = "MH Monitor";

/// Аргумент запуска через UAC из `--uninstall` без прав: вопрос уже задан, данные пользователя
/// удалит вызвавший процесс. Значение — его PID, чтобы не завершить его вместе с оверлеями.
const CALLER_ARG: &str = "--caller";

/// Коды выхода `--uninstall`, которые читает оверлей.
pub const EXIT_CANCELLED: u8 = 3;
pub const EXIT_DATA_REMOVED: u8 = 4;

/// Чем закончилось `--uninstall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uninstall {
    KeptData,
    RemovedData,
    Cancelled,
}

fn program_files() -> PathBuf {
    std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

/// `C:\Program Files\MH Monitoring`.
pub fn install_dir() -> PathBuf {
    program_files().join(APP_DIR)
}

pub fn installed_exe() -> PathBuf {
    install_dir().join(EXE_NAME)
}

/// Служба рядом с программой — на неё и ставится служба Windows.
pub fn installed_service_exe() -> PathBuf {
    install_dir().join(mh_ipc::SERVICE_EXE_NAME)
}

/// `MH-Monitoring-Service.exe`, встроенный при сборке. Пустой — сборка без службы.
static EMBEDDED_SERVICE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/service.bin"));

/// Кладёт службу в `target`: из файла рядом с `source` (сборка, распакованный архив), иначе из
/// встроенной копии.
fn place_service(source: &Path, target: &Path) -> Result<(), String> {
    let sibling = source.with_file_name(mh_ipc::SERVICE_EXE_NAME);
    if same_path(&sibling, target) {
        return if target.is_file() {
            Ok(())
        } else {
            Err("нет файла службы".to_string())
        };
    }
    if sibling.is_file() {
        return copy_atomically(&sibling, target);
    }
    if EMBEDDED_SERVICE.is_empty() {
        return Err(
            r"в эту сборку служба не встроена — соберите через tools\build-release.ps1".to_string()
        );
    }
    write_atomically(EMBEDDED_SERVICE, target)
}

/// Общее меню «Пуск» — для всех пользователей.
fn start_menu_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join(r"Microsoft\Windows\Start Menu\Programs")
}

/// Общий рабочий стол — для всех пользователей, как и «Пуск».
fn desktop_dir() -> PathBuf {
    let base = std::env::var_os("PUBLIC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\Public"));
    base.join("Desktop")
}

fn shortcut_paths() -> [PathBuf; 2] {
    [start_menu_dir().join(SHORTCUT_NAME), desktop_dir().join(SHORTCUT_NAME)]
}

/// Версия установленной копии; `None` — не установлено.
pub fn installed_version() -> Option<String> {
    if !installed_exe().is_file() {
        return None;
    }
    registry::read_text(Hive::LocalMachine, UNINSTALL_KEY, "DisplayVersion")
}

/// Запущена ли именно установленная копия.
pub fn running_installed() -> bool {
    std::env::current_exe().is_ok_and(|exe| same_path(&exe, &installed_exe()))
}

/// Пути Windows сравниваются без учёта регистра и разделителей.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let normalize = |path: &Path| path.to_string_lossy().replace('/', "\\").to_lowercase();
    normalize(a) == normalize(b)
}

/// `--install`. Требует прав администратора.
pub fn install() -> Result<(), String> {
    if !is_elevated() {
        return Err("нужны права администратора".to_string());
    }
    let source = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = install_dir();
    let target = installed_exe();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // Старая служба держит старый файл — снять её до копирования.
    service::remove_if_present()?;
    if !same_path(&source, &target) {
        // Открытые оверлеи старой установки тоже держат файл.
        for pid in processes_from(&target) {
            let _ = terminate(pid);
        }
        copy_atomically(&source, &target)?;
    }
    let service_target = installed_service_exe();
    place_service(&source, &service_target)?;
    remove_legacy_files(&source);
    service::install_at(&service_target)?;

    for link in shortcut_paths() {
        if let Err(error) = shortcut::create(&link, &target, "MH Monitoring — оверлей") {
            diag::log(format!("ярлык {} не создан: {error}", link.display()));
        }
    }
    migrate_autostart(&target);
    let version = env!("CARGO_PKG_VERSION");
    let uninstall = format!("\"{}\" --uninstall", target.display());
    let size_kb = std::fs::metadata(&target).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
    registry::write_values(
        Hive::LocalMachine,
        UNINSTALL_KEY,
        &[
            ("DisplayName", Value::Text("MH Monitoring")),
            ("DisplayVersion", Value::Text(version)),
            ("Publisher", Value::Text("midnightheartsgames")),
            ("DisplayIcon", Value::Text(&target.to_string_lossy())),
            ("InstallLocation", Value::Text(&dir.to_string_lossy())),
            ("UninstallString", Value::Text(&uninstall)),
            ("NoModify", Value::Number(1)),
            ("NoRepair", Value::Number(1)),
            ("EstimatedSize", Value::Number(size_kb)),
        ],
    )
    .map_err(|e| format!("запись в «Программы и компоненты»: {e}"))?;
    Ok(())
}

/// Старые exe, папка и ярлык. `running` — файл, из которого запущены мы: его не трогаем.
fn remove_legacy_files(running: &Path) {
    let legacy_dir = program_files().join(LEGACY_APP_DIR);
    for exe in [legacy_dir.join(LEGACY_EXE_NAME), install_dir().join(LEGACY_EXE_NAME)] {
        if same_path(&exe, running) || !exe.exists() {
            continue;
        }
        for pid in processes_from(&exe) {
            let _ = terminate(pid);
        }
        if let Err(error) = std::fs::remove_file(&exe) {
            diag::log(format!("старый файл {} не удалён: {error}", exe.display()));
        }
    }
    if legacy_dir.exists() && !running.starts_with(&legacy_dir) {
        let _ = std::fs::remove_dir_all(&legacy_dir);
    }
    let _ = std::fs::remove_file(start_menu_dir().join(LEGACY_SHORTCUT_NAME));
}

/// Включённый автозапуск переводится на установленную копию, старое имя значения убирается.
fn migrate_autostart(target: &Path) {
    let legacy = registry::read_text(Hive::CurrentUser, RUN_KEY, LEGACY_RUN_VALUE).is_some();
    if legacy {
        let _ = registry::delete_value(Hive::CurrentUser, RUN_KEY, LEGACY_RUN_VALUE);
    }
    if legacy || autostart_enabled() {
        let command = format!("\"{}\"", target.display());
        let _ = registry::write_values(
            Hive::CurrentUser,
            RUN_KEY,
            &[(RUN_VALUE, Value::Text(&command))],
        );
    }
}

/// `--uninstall`. Спрашивает про данные пользователя; без прав перезапускает себя через UAC.
///
/// Данные удаляет процесс, который задал вопрос: у процесса, запущенного через UAC, может быть
/// чужой профиль, если права дала другая учётная запись.
pub fn uninstall(arguments: &[String]) -> Result<Uninstall, String> {
    if let Some(caller) = caller_of(arguments) {
        remove_installation(Some(caller))?;
        return Ok(Uninstall::KeptData);
    }

    let remove_data = match ask(
        "MH Monitoring",
        "Удалить MH Monitoring?\n\n\
         Удалить также настройки и журналы?\n\n\
         «Да» — удалить программу и данные.\n\
         «Нет» — удалить программу, данные оставить.\n\
         «Отмена» — ничего не удалять.",
    ) {
        Answer::Yes => true,
        Answer::No => false,
        Answer::Cancel => return Ok(Uninstall::Cancelled),
    };

    if is_elevated() {
        remove_installation(None)?;
    } else {
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let arguments = format!("--uninstall {CALLER_ARG} {}", std::process::id());
        match mh_platform::elevate::run_elevated(&executable, &arguments) {
            Ok(0) => {}
            Ok(code) => return Err(format!("удаление завершилось с кодом {code}")),
            Err(error) => return Err(error.to_string()),
        }
    }
    let _ = registry::delete_value(Hive::CurrentUser, RUN_KEY, RUN_VALUE);
    let _ = registry::delete_value(Hive::CurrentUser, RUN_KEY, LEGACY_RUN_VALUE);

    let text = if remove_data {
        remove_user_data();
        "MH Monitoring удалён вместе с настройками и журналами."
    } else {
        "MH Monitoring удалён.\n\nНастройки остались в %APPDATA%\\MH Monitoring."
    };
    message("MH Monitoring", text);
    Ok(if remove_data { Uninstall::RemovedData } else { Uninstall::KeptData })
}

fn caller_of(arguments: &[String]) -> Option<u32> {
    let index = arguments.iter().position(|argument| argument == CALLER_ARG)?;
    arguments.get(index + 1)?.parse().ok()
}

/// Служба, файлы программы, ярлыки, «Программы и компоненты». Требует прав администратора.
fn remove_installation(caller: Option<u32>) -> Result<(), String> {
    if !is_elevated() {
        return Err("нужны права администратора".to_string());
    }
    let target = installed_exe();
    service::remove_if_present()?;
    for pid in processes_from(&target) {
        if Some(pid) != caller {
            let _ = terminate(pid);
        }
    }
    for link in shortcut_paths() {
        let _ = std::fs::remove_file(link);
    }
    if let Ok(own) = std::env::current_exe() {
        remove_legacy_files(&own);
    }
    let _ = registry::delete_value(Hive::CurrentUser, RUN_KEY, RUN_VALUE);
    registry::delete_key(Hive::LocalMachine, UNINSTALL_KEY)
        .map_err(|e| format!("запись в «Программы и компоненты»: {e}"))?;

    let mut paths = vec![install_dir()];
    // Копия PresentMon и журналы службы в профиле системы.
    if let Some(root) = std::env::var_os("SystemRoot") {
        paths.push(
            PathBuf::from(root).join(r"System32\config\systemprofile\AppData\Local").join(APP_DIR),
        );
    }
    // Файл программы держат этот процесс и вызвавший его — удалять после их выхода.
    let mut waiting = vec![std::process::id()];
    waiting.extend(caller);
    remove_after_exit(&paths, &waiting);
    Ok(())
}

/// Настройки и журналы текущего пользователя. Журналы открыты этим процессом и оверлеем из того
/// же файла, поэтому удаление — после их выхода.
fn remove_user_data() {
    let mut paths = vec![settings::local_dir()];
    if let Some(dir) = settings::default_path().parent() {
        paths.push(dir.to_path_buf());
    }
    let mut waiting = vec![std::process::id()];
    if let Ok(own) = std::env::current_exe() {
        waiting.extend(processes_from(&own));
    }
    remove_after_exit(&paths, &waiting);
}

/// Запускает скрытый PowerShell, который ждёт выхода `pids` и удаляет `paths`.
fn remove_after_exit(paths: &[PathBuf], pids: &[u32]) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(removal_script(paths, pids))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

fn removal_script(paths: &[PathBuf], pids: &[u32]) -> String {
    let ids = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let quoted = paths
        .iter()
        .map(|path| format!("'{}'", path.display().to_string().replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "Wait-Process -Id {ids} -Timeout 120 -ErrorAction SilentlyContinue; Start-Sleep -Seconds 1; \
         Remove-Item -LiteralPath {quoted} -Recurse -Force -ErrorAction SilentlyContinue"
    )
}

/// Запускает установленную копию и возвращает управление: вызывающий должен выйти.
pub fn launch_installed() -> std::io::Result<()> {
    std::process::Command::new(installed_exe()).arg("--after-install").spawn().map(|_| ())
}

/// Автозапуск при входе в систему — для текущего пользователя.
pub fn autostart_enabled() -> bool {
    registry::read_text(Hive::CurrentUser, RUN_KEY, RUN_VALUE).is_some()
}

pub fn set_autostart(enabled: bool) -> std::io::Result<()> {
    if enabled {
        let exe =
            if installed_version().is_some() { installed_exe() } else { std::env::current_exe()? };
        let command = format!("\"{}\"", exe.display());
        registry::write_values(Hive::CurrentUser, RUN_KEY, &[(RUN_VALUE, Value::Text(&command))])
    } else {
        registry::delete_value(Hive::CurrentUser, RUN_KEY, RUN_VALUE)
    }
}

/// Копирование через временное имя: оборванная копия не оставит битый exe на месте рабочего.
fn copy_atomically(source: &Path, target: &Path) -> Result<(), String> {
    let partial = target.with_extension("exe.partial");
    std::fs::copy(source, &partial).map_err(|e| format!("копирование: {e}"))?;
    replace_with(&partial, target)
}

fn write_atomically(bytes: &[u8], target: &Path) -> Result<(), String> {
    let partial = target.with_extension("exe.partial");
    std::fs::write(&partial, bytes).map_err(|e| format!("запись {}: {e}", partial.display()))?;
    replace_with(&partial, target)
}

fn replace_with(partial: &Path, target: &Path) -> Result<(), String> {
    std::fs::rename(partial, target).map_err(|e| {
        let _ = std::fs::remove_file(partial);
        format!("замена {}: {e}", target.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_compare_like_windows_does() {
        assert!(same_path(
            Path::new(r"C:\Program Files\MH Monitoring\MH-Monitoring.exe"),
            Path::new(r"c:/program files/mh monitoring/mh-monitoring.EXE")
        ));
        assert!(!same_path(
            Path::new(r"C:\Program Files\MH Monitoring\MH-Monitoring.exe"),
            Path::new(r"I:\IdeaProjects\MH Monitoring\target\release\MH-Monitoring.exe")
        ));
    }

    #[test]
    fn the_install_location_is_under_program_files() {
        let exe = installed_exe();
        assert!(exe.ends_with(r"MH Monitoring\MH-Monitoring.exe"), "{}", exe.display());
        assert!(exe.to_string_lossy().to_lowercase().contains("program files"));
    }

    #[test]
    fn shortcuts_go_to_start_menu_and_desktop() {
        let [menu, desktop] = shortcut_paths();
        assert!(menu.ends_with(r"Start Menu\Programs\MH Monitoring.lnk"), "{}", menu.display());
        assert!(desktop.ends_with(r"Desktop\MH Monitoring.lnk"), "{}", desktop.display());
    }

    #[test]
    fn the_caller_pid_is_read_from_arguments() {
        let arguments = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(caller_of(&arguments(&["--uninstall", "--caller", "4242"])), Some(4_242));
        assert_eq!(caller_of(&arguments(&["--uninstall"])), None);
        assert_eq!(caller_of(&arguments(&["--uninstall", "--caller"])), None);
        assert_eq!(caller_of(&arguments(&["--uninstall", "--caller", "x"])), None);
    }

    #[test]
    fn the_removal_script_waits_and_quotes_paths() {
        let script = removal_script(
            &[
                PathBuf::from(r"C:\Program Files\MH Monitoring"),
                PathBuf::from(r"C:\Users\O'Neil\x"),
            ],
            &[10, 20],
        );
        assert!(script.starts_with("Wait-Process -Id 10,20 "), "{script}");
        assert!(
            script.contains(r"-LiteralPath 'C:\Program Files\MH Monitoring','C:\Users\O''Neil\x'"),
            "{script}"
        );
    }

    #[test]
    fn an_atomic_copy_replaces_the_target() {
        let dir = std::env::temp_dir().join(format!("mh-install-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("new.exe");
        let target = dir.join("MH-Monitoring.exe");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&target, b"old").unwrap();
        copy_atomically(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!target.with_extension("exe.partial").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_service_is_taken_from_a_sibling_file_first() {
        let dir = std::env::temp_dir().join(format!("mh-install-svc-{}", std::process::id()));
        let target_dir = dir.join("installed");
        std::fs::create_dir_all(&target_dir).unwrap();
        let source = dir.join("MH-Monitoring.exe");
        std::fs::write(dir.join(mh_ipc::SERVICE_EXE_NAME), b"service").unwrap();
        let target = target_dir.join(mh_ipc::SERVICE_EXE_NAME);
        place_service(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"service");
        // Установленная копия переустанавливает себя: файл службы уже на месте.
        place_service(&target_dir.join("MH-Monitoring.exe"), &target).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_installed_service_sits_next_to_the_program() {
        assert_eq!(installed_service_exe().parent(), installed_exe().parent());
    }
}

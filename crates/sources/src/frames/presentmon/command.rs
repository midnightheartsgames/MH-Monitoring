//! Построение командной строки PresentMon.
//!
//! Модуль маленький, но каждая строчка в нём — требование из PLAN.md §2.1, и нарушение любой
//! воспроизводит баг, который ломает захват кадров на всей машине.

use std::path::{Path, PathBuf};

// Имя сессии и правило её уборки живут в `mh_core::session_name` — одним определением на
// оба применения. Разъехавшись, они уже один раз стоили вечера отладки.
pub use mh_core::session_name::{SESSION_PREFIX, session_name};

/// Что запускать и с какими аргументами.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCommand {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
}

/// Откуда берётся исполняемый файл.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutableChoice {
    /// Вложенный в дистрибутив (решение D6).
    Bundled(PathBuf),
    /// Явно указанный пользователем — имеет приоритет над вложенным.
    UserOverride(PathBuf),
}

impl ExecutableChoice {
    pub fn path(&self) -> &Path {
        match self {
            ExecutableChoice::Bundled(path) | ExecutableChoice::UserOverride(path) => path,
        }
    }

    /// Выбор: пользовательский путь поверх вложенного, если он задан и непустой.
    pub fn resolve(bundled: PathBuf, user_override: Option<PathBuf>) -> ExecutableChoice {
        match user_override {
            Some(path) if !path.as_os_str().is_empty() => ExecutableChoice::UserOverride(path),
            _ => ExecutableChoice::Bundled(bundled),
        }
    }
}

/// Собирает команду захвата для одного процесса-цели.
///
/// Разбор флагов:
///
/// * `--process_id`, а не `--process_name`: две копии одного exe иначе неразличимы (§2.6);
/// * `--output_stdout` + `--no_console_stats` — CSV в трубу и никакой интерактивной статистики;
/// * `--terminate_on_proc_exit` — PresentMon уходит вместе с игрой, а не висит;
/// * `--session_name` — наше имя, одно на запуск приложения;
/// * `--stop_existing_session` безопасен **только потому, что имя наше**. С чужим именем он
///   остановил бы чужой захват.
///
/// Чего здесь нарочно нет: `--v1_metrics` и `--v2_metrics`. Они пинят набор метрик и ломают
/// совместимость со сборками, где их нет; схема и так определяется по заголовку CSV (§2.6).
pub fn build(
    executable: &ExecutableChoice,
    session_name: &str,
    target_process_id: u32,
) -> CaptureCommand {
    CaptureCommand {
        executable: executable.path().to_path_buf(),
        arguments: vec![
            "--output_stdout".to_string(),
            "--no_console_stats".to_string(),
            "--terminate_on_proc_exit".to_string(),
            "--session_name".to_string(),
            session_name.to_string(),
            "--stop_existing_session".to_string(),
            "--process_id".to_string(),
            target_process_id.to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled() -> ExecutableChoice {
        ExecutableChoice::Bundled(PathBuf::from("assets/presentmon/PresentMon-2.5.1-x64.exe"))
    }

    #[test]
    fn the_session_name_carries_our_prefix_and_our_pid() {
        assert_eq!(session_name(31_504), "MHMonitor-31504");
        assert!(session_name(1).starts_with(SESSION_PREFIX));
    }

    /// Имя по умолчанию `PresentMon` общее для чужих инструментов — своим оно быть не может.
    #[test]
    fn the_session_name_is_never_the_shared_default() {
        assert_ne!(session_name(42), "PresentMon");
        assert!(!session_name(42).eq_ignore_ascii_case("presentmon"));
    }

    /// Одно имя на запуск приложения: две цели подряд обязаны получить одно и то же имя, иначе
    /// каждая смена цели оставит по сироте.
    #[test]
    fn switching_targets_reuses_the_same_session_name() {
        let name = session_name(31_504);
        let first = build(&bundled(), &name, 4_242);
        let second = build(&bundled(), &name, 777);

        let session_of = |command: &CaptureCommand| {
            let at = command.arguments.iter().position(|a| a == "--session_name").unwrap();
            command.arguments[at + 1].clone()
        };
        assert_eq!(session_of(&first), session_of(&second));
        assert_eq!(session_of(&first), "MHMonitor-31504");
    }

    #[test]
    fn the_target_is_addressed_by_pid_not_by_name() {
        let command = build(&bundled(), "MHMonitor-1", 4_242);
        assert!(command.arguments.contains(&"--process_id".to_string()));
        assert!(command.arguments.contains(&"4242".to_string()));
        assert!(
            !command.arguments.contains(&"--process_name".to_string()),
            "две копии одного exe по имени неразличимы"
        );
    }

    #[test]
    fn the_command_carries_every_required_flag() {
        let command = build(&bundled(), "MHMonitor-1", 4_242);
        for flag in [
            "--output_stdout",
            "--no_console_stats",
            "--terminate_on_proc_exit",
            "--session_name",
            "--stop_existing_session",
            "--process_id",
        ] {
            assert!(command.arguments.contains(&flag.to_string()), "нет флага {flag}");
        }
    }

    /// Они пинят набор метрик и ломают совместимость со сборками, где их нет (§2.6).
    #[test]
    fn metric_pinning_flags_are_never_passed() {
        let command = build(&bundled(), "MHMonitor-1", 4_242);
        assert!(!command.arguments.contains(&"--v1_metrics".to_string()));
        assert!(!command.arguments.contains(&"--v2_metrics".to_string()));
    }

    #[test]
    fn a_user_path_wins_over_the_bundled_one() {
        let choice = ExecutableChoice::resolve(
            PathBuf::from("bundled.exe"),
            Some(PathBuf::from(r"C:\Tools\PresentMon.exe")),
        );
        assert_eq!(
            choice,
            ExecutableChoice::UserOverride(PathBuf::from(r"C:\Tools\PresentMon.exe"))
        );
        assert_eq!(choice.path(), Path::new(r"C:\Tools\PresentMon.exe"));
    }

    #[test]
    fn an_absent_or_empty_override_falls_back_to_the_bundled_one() {
        let bundled = PathBuf::from("bundled.exe");
        assert_eq!(
            ExecutableChoice::resolve(bundled.clone(), None),
            ExecutableChoice::Bundled(bundled.clone())
        );
        assert_eq!(
            ExecutableChoice::resolve(bundled.clone(), Some(PathBuf::new())),
            ExecutableChoice::Bundled(bundled)
        );
    }
}

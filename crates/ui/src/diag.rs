//! Короткий журнал последнего запуска: `%LOCALAPPDATA%\MH Monitor\mh-monitor.log`.
//!
//! Только ключевые шаги со временем от старта — чтобы по письму пользователя было видно, где
//! уходят секунды (открытие настроек, выход). Полноценное журналирование — фаза P7.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Journal {
    started: Instant,
    file: Option<std::fs::File>,
}

static JOURNAL: OnceLock<Mutex<Journal>> = OnceLock::new();

/// Открывает журнал заново. Ошибка записи не мешает работе: журнал — подсказка, не функция.
pub fn start(path: PathBuf) {
    let file = path
        .parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .ok()
        .and_then(|_| std::fs::File::create(&path).ok());
    let _ = JOURNAL.set(Mutex::new(Journal { started: Instant::now(), file }));
}

pub fn log(message: impl AsRef<str>) {
    let Some(journal) = JOURNAL.get() else { return };
    let mut journal = journal.lock().unwrap_or_else(|e| e.into_inner());
    let elapsed = journal.started.elapsed();
    if let Some(file) = journal.file.as_mut() {
        let _ = writeln!(
            file,
            "[{:>6}.{:03} с] {}",
            elapsed.as_secs(),
            elapsed.subsec_millis(),
            message.as_ref()
        );
        let _ = file.flush();
    }
}

/// Меряет, сколько занял шаг, и пишет это в журнал.
pub fn timed<T>(what: &str, step: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = step();
    log(format!("{what}: {} мс", started.elapsed().as_millis()));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_records_steps_with_durations() {
        let path = std::env::temp_dir()
            .join(format!("mh-ui-diag-{}", std::process::id()))
            .join("mh-monitor.log");
        start(path.clone());
        log("старт");
        assert_eq!(timed("шаг", || 7), 7);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("старт"));
        assert!(text.contains("шаг: "));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

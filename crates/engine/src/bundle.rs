//! Вложенный PresentMon (решение D6).
//!
//! Приложение — один исполняемый файл (PLAN.md §3), а PresentMon запускается отдельным процессом.
//! Поэтому его байты встроены сюда и при старте раскладываются в `%LOCALAPPDATA%`.

use std::io;
use std::path::{Path, PathBuf};

/// Версия зафиксирована; происхождение и хеш — `assets/presentmon/NOTICE.md`.
const FILE_NAME: &str = "PresentMon-2.5.1-x64.exe";
const BYTES: &[u8] = include_bytes!("../../../assets/presentmon/PresentMon-2.5.1-x64.exe");

/// Кладёт PresentMon в `dir` и возвращает путь к нему.
///
/// Файл переписывается, только если отличается: иначе второй экземпляр приложения, у которого
/// PresentMon уже запущен, получил бы отказ в доступе на ровном месте.
pub fn extract_presentmon(dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let current = std::fs::read(&path).ok();
    if current.as_deref() != Some(BYTES) {
        // Сначала во временный файл, потом переименование: оборванная запись не оставит
        // полуфайл, который потом будет «запускаться».
        let partial = dir.join(format!("{FILE_NAME}.partial"));
        std::fs::write(&partial, BYTES)?;
        std::fs::rename(&partial, &path)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_is_idempotent_and_repairs_a_damaged_copy() {
        let dir = std::env::temp_dir().join(format!("mh-engine-bundle-{}", std::process::id()));
        let path = extract_presentmon(&dir).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), BYTES);

        // Второй раз — без записи: время изменения не меняется.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        extract_presentmon(&dir).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);

        std::fs::write(&path, b"broken").unwrap();
        extract_presentmon(&dir).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), BYTES);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

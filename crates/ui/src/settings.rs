//! Настройки оверлея на диске.
//!
//! В P4 — только то, без чего оверлей не работает: позиция, блокировка, видимость, вид и хоткеи.
//! Полные настройки — фаза P5. Любое отсутствующее или испорченное поле берёт значение по
//! умолчанию: сломанный файл не должен мешать запуску.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    /// Левый верхний угол окна в физических пикселях экрана.
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// Непрозрачность **только фона**; текст всегда непрозрачен (PLAN.md §6/P4).
    pub opacity: f32,
    /// Масштаб HUD поверх масштаба Windows: 0.75 / 1.0 / 1.25 / 1.5.
    pub scale: f32,
    /// Заблокирован — значит прозрачен для мыши: мышь принадлежит игре.
    pub locked: bool,
    pub visible: bool,
    pub show_graph: bool,
    pub show_header: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            x: None,
            y: None,
            opacity: 0.85,
            scale: 1.0,
            locked: false,
            visible: true,
            show_graph: true,
            show_header: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeySettings {
    pub toggle_visibility: String,
    pub toggle_lock: String,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self { toggle_visibility: "Ctrl+Shift+F11".into(), toggle_lock: "Ctrl+Shift+F10".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub overlay: OverlaySettings,
    pub hotkeys: HotkeySettings,
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
            .map(Settings::sanitized)
            .unwrap_or_default()
    }

    /// Запись через временный файл: оборванная запись не оставит полуфайл.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, text)?;
        std::fs::rename(&partial, path)
    }

    /// Значения, с которыми HUD был бы невидим или нечитаем, заменяются разумными.
    fn sanitized(mut self) -> Settings {
        let defaults = OverlaySettings::default();
        let overlay = &mut self.overlay;
        if !overlay.opacity.is_finite() {
            overlay.opacity = defaults.opacity;
        }
        overlay.opacity = overlay.opacity.clamp(0.0, 1.0);
        if !overlay.scale.is_finite() {
            overlay.scale = defaults.scale;
        }
        overlay.scale = overlay.scale.clamp(0.5, 2.0);
        self
    }
}

/// `%APPDATA%\MH Monitor\settings-rs.json`.
///
/// **Не `settings.json`**: этот файл принадлежит Kotlin-версии, у него другая схема, и запись
/// поверх уже однажды стёрла её настройки.
pub fn default_path() -> PathBuf {
    app_dir(std::env::var_os("APPDATA")).join("settings-rs.json")
}

/// `%LOCALAPPDATA%\MH Monitor` — сюда раскладываются вложенные программы.
pub fn local_dir() -> PathBuf {
    app_dir(std::env::var_os("LOCALAPPDATA"))
}

fn app_dir(base: Option<std::ffi::OsString>) -> PathBuf {
    base.map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("MH Monitor")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mh-ui-{}-{name}.json", std::process::id()))
    }

    #[test]
    fn a_missing_or_broken_file_gives_defaults() {
        assert_eq!(Settings::load(&temp_file("missing")), Settings::default());
        let broken = temp_file("broken");
        std::fs::write(&broken, "{ not json").unwrap();
        assert_eq!(Settings::load(&broken), Settings::default());
        std::fs::remove_file(broken).unwrap();
    }

    #[test]
    fn missing_fields_take_defaults() {
        let partial = temp_file("partial");
        std::fs::write(&partial, r#"{ "overlay": { "locked": true } }"#).unwrap();
        let settings = Settings::load(&partial);
        assert!(settings.overlay.locked);
        assert_eq!(settings.overlay.opacity, OverlaySettings::default().opacity);
        assert_eq!(settings.hotkeys, HotkeySettings::default());
        std::fs::remove_file(partial).unwrap();
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let path = temp_file("roundtrip");
        let mut settings = Settings::default();
        settings.overlay.x = Some(-1_900);
        settings.overlay.y = Some(40);
        settings.overlay.locked = true;
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn absurd_values_are_clamped() {
        let path = temp_file("absurd");
        std::fs::write(&path, r#"{ "overlay": { "opacity": 7.0, "scale": 0.01 } }"#).unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.overlay.opacity, 1.0);
        assert_eq!(settings.overlay.scale, 0.5);
        std::fs::remove_file(path).unwrap();
    }
}

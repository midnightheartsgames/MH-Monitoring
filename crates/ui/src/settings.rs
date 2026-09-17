//! Настройки на диске (PLAN.md §6/P5).
//!
//! Правила:
//! * **Никакой файл не мешает запуску.** Непарсящийся JSON уходит в карантин рядом, дальше —
//!   умолчания. Валидный JSON с абсурдными значениями чинится после загрузки.
//! * **Версия схемы в файле.** Файл без версии — формат P4; миграции идут по цепочке до текущей.
//!   Файл от более новой сборки читается как есть: незнакомые поля пропускаются.
//! * **Запись атомарная** — через временный файл и переименование.
//! * **Путь свой.** `%APPDATA%\MH Monitoring\settings-rs.json`; `settings.json` в старой папке
//!   `MH Monitor` принадлежит Kotlin-версии (PLAN.md §2.16). Файл Rust-версии из старой папки
//!   переносится при запуске ([`migrate_legacy_dir`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mh_core::{TargetMode, TargetSettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Текущая версия схемы. Меняется вместе с добавлением шага в [`migrate`].
pub const CURRENT_VERSION: u32 = 2;

/// Допустимые масштабы HUD.
pub const SCALES: [f32; 4] = [0.75, 1.0, 1.25, 1.5];
pub const MIN_OPACITY: f32 = 0.3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    /// Левый верхний угол окна в физических пикселях экрана.
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// Непрозрачность **только фона**; текст всегда непрозрачен (PLAN.md §6/P4).
    pub opacity: f32,
    /// Масштаб HUD поверх масштаба Windows, одно из [`SCALES`].
    pub scale: f32,
    /// Заблокирован — значит прозрачен для мыши: мышь принадлежит игре.
    pub locked: bool,
    pub visible: bool,
    pub always_on_top: bool,
    pub show_graph: bool,
    pub show_header: bool,
    /// Что делать с HUD, пока игра в эксклюзивном полноэкранном режиме.
    #[serde(deserialize_with = "lenient_fullscreen")]
    pub fullscreen: FullscreenMode,
}

/// Поверх эксклюзивного полноэкранного режима окна не видны (PLAN.md §6/P9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullscreenMode {
    /// Спрятать HUD, пока игра в этом режиме.
    #[default]
    Hide,
    /// Перенести HUD на монитор без игры; если он один — спрятать.
    OtherMonitor,
}

/// Незнакомое значение от другой сборки — умолчание, а не карантин всего файла.
fn lenient_fullscreen<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<FullscreenMode, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(FullscreenMode::deserialize(value).unwrap_or_default())
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
            always_on_top: true,
            show_graph: true,
            show_header: true,
            fullscreen: FullscreenMode::Hide,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SectionSettings {
    pub gpu: bool,
    pub cpu: bool,
    pub ram: bool,
    pub fps: bool,
}

impl Default for SectionSettings {
    fn default() -> Self {
        Self { gpu: true, cpu: true, ram: true, fps: true }
    }
}

/// Строки HUD, которые можно выключить.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Metric {
    GpuTemperature,
    GpuClock,
    GpuVram,
    GpuPower,
    GpuFan,
    CpuTemperature,
    CpuClock,
    CpuPower,
    RamUsed,
    FpsAverage,
    FpsLow1,
    FpsLow01,
}

impl Metric {
    #[cfg(test)]
    pub const ALL: [Metric; 12] = [
        Metric::GpuTemperature,
        Metric::GpuClock,
        Metric::GpuVram,
        Metric::GpuPower,
        Metric::GpuFan,
        Metric::CpuTemperature,
        Metric::CpuClock,
        Metric::CpuPower,
        Metric::RamUsed,
        Metric::FpsAverage,
        Metric::FpsLow1,
        Metric::FpsLow01,
    ];

    /// Имя в файле. Храним строками, а не перечислением serde: одно незнакомое имя от другой
    /// сборки иначе сломало бы разбор всего файла.
    pub fn key(self) -> &'static str {
        match self {
            Metric::GpuTemperature => "gpu_temperature",
            Metric::GpuClock => "gpu_clock",
            Metric::GpuVram => "gpu_vram",
            Metric::GpuPower => "gpu_power",
            Metric::GpuFan => "gpu_fan",
            Metric::CpuTemperature => "cpu_temperature",
            Metric::CpuClock => "cpu_clock",
            Metric::CpuPower => "cpu_power",
            Metric::RamUsed => "ram_used",
            Metric::FpsAverage => "fps_average",
            Metric::FpsLow1 => "fps_low_1",
            Metric::FpsLow01 => "fps_low_0_1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricSettings {
    /// Выключенные строки. Всё, чего здесь нет, включено.
    pub disabled: BTreeSet<String>,
    /// Строка, которую машина ни разу не сообщила, исчезает вместо прочерка.
    ///
    /// Выключено по умолчанию: HUD с постоянным набором строк читается легче, чем HUD, чья
    /// раскладка зависит от того, какие датчики ответили.
    pub hide_unavailable: bool,
}

impl MetricSettings {
    pub fn is_enabled(&self, metric: Metric) -> bool {
        !self.disabled.contains(metric.key())
    }

    pub fn set_enabled(&mut self, metric: Metric, enabled: bool) {
        if enabled {
            self.disabled.remove(metric.key());
        } else {
            self.disabled.insert(metric.key().to_string());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetChoice {
    /// Окно в фокусе.
    #[default]
    Auto,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FpsSettings {
    pub target: TargetChoice,
    pub manual_process: Option<String>,
    /// Нужен, когда запущено несколько копий одного exe; важнее имени.
    pub manual_pid: Option<u32>,
    /// Свой PresentMon.exe вместо вложенного. Применяется после перезапуска.
    pub presentmon_path: Option<String>,
}

impl FpsSettings {
    pub fn target_settings(&self) -> TargetSettings {
        match self.target {
            TargetChoice::Auto => TargetSettings::auto(),
            TargetChoice::Manual => TargetSettings {
                mode: TargetMode::Manual,
                manual_process: self.manual_process.clone(),
                manual_pid: self.manual_pid,
            },
        }
    }
}

/// Игра, которую программа запускает сама (PLAN.md §6/P10).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameProfile {
    /// Имя в настройках и в `--launch "<имя>"`. Уникально.
    pub name: String,
    /// Что запускать: сама игра или её загрузчик.
    pub path: String,
    /// Параметры запуска, например `-window`.
    pub arguments: String,
    /// Снимать рамку с окна игры и растягивать его на монитор.
    pub borderless: bool,
    /// Файл, чьё окно делать без рамки, если это не `path`: загрузчик Warcraft III
    /// `Frozen Throne.exe` запускает `war3.exe` и выходит.
    pub window_exe: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub overlay: OverlaySettings,
    pub sections: SectionSettings,
    pub metrics: MetricSettings,
    pub fps: FpsSettings,
    pub hotkeys: HotkeySettings,
    /// Окно первого запуска пройдено: выбрана установка или работа без прав.
    pub setup_done: bool,
    pub games: Vec<GameProfile>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            overlay: OverlaySettings::default(),
            sections: SectionSettings::default(),
            metrics: MetricSettings::default(),
            fps: FpsSettings::default(),
            hotkeys: HotkeySettings::default(),
            setup_done: false,
            games: Vec::new(),
        }
    }
}

/// Итог загрузки: настройки и, если было что сказать, сообщение для пользователя.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub settings: Settings,
    pub notice: Option<String>,
}

impl Settings {
    pub fn load(path: &Path) -> Loaded {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => return Loaded { settings: Settings::default(), notice: None },
        };
        match parse(&text) {
            Ok((settings, notice)) => Loaded { settings, notice },
            Err(()) => {
                let aside = quarantine_path(path);
                let notice = match std::fs::rename(path, &aside) {
                    Ok(()) => format!(
                        "настройки повреждены и сброшены; старый файл — {}",
                        aside.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                    ),
                    Err(_) => "настройки повреждены и сброшены".to_string(),
                };
                Loaded { settings: Settings::default(), notice: Some(notice) }
            }
        }
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

    /// Значения, с которыми HUD был бы невидим, нечитаем или ронял бы захват, заменяются.
    pub fn sanitized(mut self) -> Settings {
        let defaults = OverlaySettings::default();
        let overlay = &mut self.overlay;
        overlay.opacity = if overlay.opacity.is_finite() {
            overlay.opacity.clamp(MIN_OPACITY, 1.0)
        } else {
            defaults.opacity
        };
        // Масштаб — к ближайшему допустимому: 1.1 из старого файла — это 100 %, а не ошибка.
        overlay.scale = if overlay.scale.is_finite() {
            SCALES
                .into_iter()
                .min_by(|a, b| (a - overlay.scale).abs().total_cmp(&(b - overlay.scale).abs()))
                .unwrap_or(defaults.scale)
        } else {
            defaults.scale
        };

        let fps = &mut self.fps;
        fps.manual_process = non_blank(fps.manual_process.take());
        fps.presentmon_path = non_blank(fps.presentmon_path.take());
        fps.manual_pid = fps.manual_pid.filter(|pid| *pid > 0);

        let hotkeys = &mut self.hotkeys;
        let default_hotkeys = HotkeySettings::default();
        if hotkeys.toggle_visibility.trim().is_empty() {
            hotkeys.toggle_visibility = default_hotkeys.toggle_visibility;
        }
        if hotkeys.toggle_lock.trim().is_empty() {
            hotkeys.toggle_lock = default_hotkeys.toggle_lock;
        }

        // Профиль без пути запускать нечего; имя нужно для `--launch`.
        self.games.retain(|game| !game.path.trim().is_empty());
        let mut names = BTreeSet::new();
        for game in &mut self.games {
            game.path = game.path.trim().to_string();
            game.window_exe = non_blank(game.window_exe.take());
            let base = crate::games::clean_name(&game.name)
                .unwrap_or_else(|| crate::games::name_from_path(&game.path));
            game.name = crate::games::unique_name(&base, |name| names.contains(name));
            names.insert(game.name.to_lowercase());
        }

        self.version = self.version.max(CURRENT_VERSION);
        self
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
}

/// Разбор текста файла: миграция, чтение, починка. `Err` — файл в карантин.
fn parse(text: &str) -> Result<(Settings, Option<String>), ()> {
    let mut value: Value = serde_json::from_str(text).map_err(|_| ())?;
    if !value.is_object() {
        return Err(());
    }
    let version = value.get("version").and_then(Value::as_u64).unwrap_or(0) as u32;
    let notice = (version > CURRENT_VERSION).then(|| {
        format!(
            "настройки от более новой версии ({version}); незнакомые поля будут потеряны при \
             сохранении"
        )
    });
    migrate(&mut value, version);
    let settings: Settings = serde_json::from_value(value).map_err(|_| ())?;
    Ok((settings.sanitized(), notice))
}

/// Приводит JSON к текущей схеме, шаг за шагом.
fn migrate(value: &mut Value, from: u32) {
    // v0 → v1: формат P4. Поля те же, добавились разделы со значениями по умолчанию —
    // их дают `serde(default)`. Нужна только метка версии.
    if from < 1 {
        value["version"] = Value::from(1);
    }
    // v1 → v2: окно первого запуска. Файл настроек уже есть — значит, программой пользовались,
    // и окно им не нужно.
    if from < 2 {
        value["setup_done"] = Value::from(true);
        value["version"] = Value::from(2);
    }
}

/// `settings-rs.json` → `settings-rs.corrupted.json`. Не `settings.corrupted.json`: это имя
/// карантина Kotlin-версии.
fn quarantine_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("settings");
    path.with_file_name(format!("{stem}.corrupted.json"))
}

const APP_DIR: &str = "MH Monitoring";
/// Папка до переименования. В `%APPDATA%` её делит Kotlin-версия, поэтому переносятся только
/// файлы Rust-версии, а папка остаётся.
const LEGACY_APP_DIR: &str = "MH Monitor";
const SETTINGS_FILE: &str = "settings-rs.json";

/// `%APPDATA%\MH Monitoring\settings-rs.json`.
///
/// **Не `settings.json`**: это имя занято Kotlin-версией, у её файла другая схема, и запись
/// поверх уже однажды стёрла её настройки.
pub fn default_path() -> PathBuf {
    app_dir(std::env::var_os("APPDATA")).join(SETTINGS_FILE)
}

/// `%LOCALAPPDATA%\MH Monitoring` — сюда раскладываются вложенные программы и журналы.
pub fn local_dir() -> PathBuf {
    app_dir(std::env::var_os("LOCALAPPDATA"))
}

fn app_dir(base: Option<std::ffi::OsString>) -> PathBuf {
    base.map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(APP_DIR)
}

/// Переносит настройки из `%APPDATA%\MH Monitor` в новую папку. Вызывать до чтения настроек.
/// Возвращает, что перенесено, — для журнала.
pub fn migrate_legacy_dir() -> std::io::Result<Vec<String>> {
    let Some(base) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return Ok(Vec::new());
    };
    move_legacy_files(&base.join(LEGACY_APP_DIR), &base.join(APP_DIR))
}

/// Файл, который уже есть в новой папке, главнее старого: старый тогда не трогается.
fn move_legacy_files(old_dir: &Path, new_dir: &Path) -> std::io::Result<Vec<String>> {
    let settings = Path::new(SETTINGS_FILE);
    let quarantine = quarantine_path(settings);
    let mut moved = Vec::new();
    for name in [settings, quarantine.as_path()] {
        let (from, to) = (old_dir.join(name), new_dir.join(name));
        if !from.is_file() || to.exists() {
            continue;
        }
        std::fs::create_dir_all(new_dir)?;
        if std::fs::rename(&from, &to).is_err() {
            // Другой том (перенаправленный профиль) — копия и удаление.
            std::fs::copy(&from, &to)?;
            std::fs::remove_file(&from)?;
        }
        moved.push(name.display().to_string());
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mh-ui-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings-rs.json")
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn legacy_settings_move_and_kotlin_files_stay() {
        let root = std::env::temp_dir().join(format!("mh-ui-{}-legacy", std::process::id()));
        let (old, new) = (root.join("MH Monitor"), root.join("MH Monitoring"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings-rs.json"), "rust").unwrap();
        std::fs::write(old.join("settings.json"), "kotlin").unwrap();

        let moved = move_legacy_files(&old, &new).unwrap();
        assert_eq!(moved, vec!["settings-rs.json".to_string()]);
        assert_eq!(std::fs::read_to_string(new.join("settings-rs.json")).unwrap(), "rust");
        assert!(!old.join("settings-rs.json").exists());
        assert!(old.join("settings.json").exists(), "файл Kotlin-версии не трогаем");
        assert!(!new.join("settings.json").exists());

        // Повторный запуск ничего не делает.
        assert!(move_legacy_files(&old, &new).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_new_settings_win_over_legacy() {
        let root = std::env::temp_dir().join(format!("mh-ui-{}-legacy-both", std::process::id()));
        let (old, new) = (root.join("MH Monitor"), root.join("MH Monitoring"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("settings-rs.json"), "old").unwrap();
        std::fs::write(new.join("settings-rs.json"), "new").unwrap();

        assert!(move_legacy_files(&old, &new).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(new.join("settings-rs.json")).unwrap(), "new");
        assert!(old.join("settings-rs.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_file_gives_defaults_silently() {
        let loaded = Settings::load(&temp_file("missing"));
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.notice, None);
    }

    #[test]
    fn a_broken_file_is_quarantined_and_reported() {
        let path = temp_file("broken");
        std::fs::write(&path, "{ not json").unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.notice.unwrap().contains("settings-rs.corrupted.json"));
        assert!(!path.exists());
        let aside = path.with_file_name("settings-rs.corrupted.json");
        assert_eq!(std::fs::read_to_string(aside).unwrap(), "{ not json");
        cleanup(&path);
    }

    #[test]
    fn a_wrong_type_is_quarantined_too() {
        let path = temp_file("wrong-type");
        std::fs::write(&path, r#"{ "overlay": { "opacity": "полупрозрачно" } }"#).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.notice.is_some());
        cleanup(&path);
    }

    #[test]
    fn a_json_array_is_not_settings() {
        assert!(parse("[1, 2]").is_err());
    }

    /// Файл, записанный сборкой P4: без версии и без новых разделов.
    #[test]
    fn a_p4_file_migrates_keeping_its_values() {
        let p4 = r#"{
          "overlay": { "x": -1800, "y": 40, "opacity": 0.9502451, "scale": 1.0, "locked": true,
                       "visible": true, "show_graph": true, "show_header": true },
          "hotkeys": { "toggle_visibility": "Ctrl+Shift+F11", "toggle_lock": "Ctrl+Shift+F10" }
        }"#;
        let (settings, notice) = parse(p4).unwrap();
        assert_eq!(notice, None);
        assert_eq!(settings.version, CURRENT_VERSION);
        assert_eq!(settings.overlay.x, Some(-1_800));
        assert!(settings.overlay.locked);
        assert!(settings.overlay.always_on_top, "новое поле берёт умолчание");
        assert_eq!(settings.sections, SectionSettings::default());
    }

    #[test]
    fn a_newer_file_loads_with_a_notice() {
        let (settings, notice) =
            parse(r#"{ "version": 9, "overlay": { "locked": true }, "future": 1 }"#).unwrap();
        assert!(settings.overlay.locked);
        assert!(notice.unwrap().contains("более новой"));
    }

    #[test]
    fn absurd_values_are_repaired_not_fatal() {
        let (settings, _) = parse(
            r#"{ "overlay": { "opacity": 7.0, "scale": 1.1 },
                 "fps": { "manual_process": "   ", "manual_pid": 0, "presentmon_path": "" },
                 "hotkeys": { "toggle_lock": " " } }"#,
        )
        .unwrap();
        assert_eq!(settings.overlay.opacity, 1.0);
        assert_eq!(settings.overlay.scale, 1.0);
        assert_eq!(settings.fps.manual_process, None);
        assert_eq!(settings.fps.manual_pid, None);
        assert_eq!(settings.fps.presentmon_path, None);
        assert_eq!(settings.hotkeys.toggle_lock, HotkeySettings::default().toggle_lock);

        let (settings, _) = parse(r#"{ "overlay": { "opacity": 0.0, "scale": 9.0 } }"#).unwrap();
        assert_eq!(settings.overlay.opacity, MIN_OPACITY, "невидимый HUD — не настройка");
        assert_eq!(settings.overlay.scale, 1.5);
    }

    /// Отрицательный PID — не число для u32: такой файл уходит в карантин, а не роняет запуск.
    #[test]
    fn a_negative_pid_does_not_crash() {
        let path = temp_file("negative-pid");
        std::fs::write(&path, r#"{ "fps": { "manual_pid": -5 } }"#).unwrap();
        assert_eq!(Settings::load(&path).settings, Settings::default());
        cleanup(&path);
    }

    #[test]
    fn unknown_metric_names_are_kept_and_harmless() {
        let (mut settings, _) =
            parse(r#"{ "metrics": { "disabled": ["gpu_fan", "from_the_future"] } }"#).unwrap();
        assert!(!settings.metrics.is_enabled(Metric::GpuFan));
        assert!(settings.metrics.is_enabled(Metric::GpuPower));
        settings.metrics.set_enabled(Metric::GpuFan, true);
        assert!(settings.metrics.is_enabled(Metric::GpuFan));
        assert!(settings.metrics.disabled.contains("from_the_future"));
    }

    #[test]
    fn metric_keys_are_unique() {
        let keys: BTreeSet<&str> = Metric::ALL.iter().map(|m| m.key()).collect();
        assert_eq!(keys.len(), Metric::ALL.len());
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let path = temp_file("roundtrip");
        let mut settings = Settings::default();
        settings.overlay.x = Some(-1_900);
        settings.overlay.locked = true;
        settings.sections.ram = false;
        settings.metrics.set_enabled(Metric::CpuPower, false);
        settings.fps.target = TargetChoice::Manual;
        settings.fps.manual_process = Some("dmc4.exe".into());
        settings.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.settings, settings);
        assert_eq!(loaded.notice, None);
        cleanup(&path);
    }

    #[test]
    fn the_target_follows_the_choice() {
        let mut fps = FpsSettings {
            manual_process: Some("dmc4.exe".into()),
            manual_pid: Some(42),
            ..Default::default()
        };
        assert_eq!(fps.target_settings(), TargetSettings::auto(), "в авто ручные поля не мешают");
        fps.target = TargetChoice::Manual;
        let target = fps.target_settings();
        assert_eq!(target.mode, TargetMode::Manual);
        assert_eq!(target.manual_process.as_deref(), Some("dmc4.exe"));
        assert_eq!(target.manual_pid, Some(42));
    }

    #[test]
    fn the_fullscreen_mode_survives_and_an_unknown_one_falls_back() {
        let (settings, _) =
            parse(r#"{ "overlay": { "fullscreen": "other_monitor" }, "setup_done": true }"#)
                .unwrap();
        assert_eq!(settings.overlay.fullscreen, FullscreenMode::OtherMonitor);
        assert!(settings.setup_done);
        let (settings, _) =
            parse(r#"{ "overlay": { "fullscreen": "from_the_future", "locked": true } }"#).unwrap();
        assert_eq!(settings.overlay.fullscreen, FullscreenMode::Hide);
        assert!(settings.overlay.locked, "остальные поля не теряются");
    }

    #[test]
    fn a_file_before_the_setup_window_counts_as_set_up() {
        // Кто уже пользовался программой, окно первого запуска не видит.
        let (settings, _) = parse(r#"{ "version": 1, "overlay": { "locked": true } }"#).unwrap();
        assert!(settings.setup_done);
    }

    #[test]
    fn game_profiles_are_repaired() {
        let (settings, _) = parse(
            r#"{ "games": [
                { "name": "WC3", "path": " C:\\Games\\WC3\\Frozen Throne.exe ",
                  "arguments": "-window", "borderless": true, "window_exe": "war3.exe" },
                { "name": "wc3", "path": "C:\\Other\\wc3.exe" },
                { "name": "без пути", "path": "  " },
                { "path": "C:\\Games\\Doom\\doom.exe", "window_exe": " " }
            ] }"#,
        )
        .unwrap();
        let names: Vec<&str> = settings.games.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["WC3", "wc3 (2)", "doom"]);
        assert_eq!(settings.games[0].path, r"C:\Games\WC3\Frozen Throne.exe");
        assert!(settings.games[0].borderless);
        assert_eq!(settings.games[2].window_exe, None);
    }

    #[test]
    fn the_quarantine_name_never_collides_with_the_kotlin_one() {
        let aside = quarantine_path(Path::new(r"C:\x\MH Monitoring\settings-rs.json"));
        assert_eq!(aside.file_name().unwrap(), "settings-rs.corrupted.json");
    }
}

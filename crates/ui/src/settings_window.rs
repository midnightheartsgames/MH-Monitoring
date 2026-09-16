//! Окно настроек (PLAN.md §6/P5).
//!
//! Флажки, ползунки и переключатели применяются сразу — их видно на HUD. Текстовые поля живут
//! черновиком и применяются кнопкой или Enter: иначе каждая набранная буква меняла бы цель и
//! перезапускала захват.

use std::path::Path;

use eframe::egui::{
    self, Color32, Pos2, RichText, ScrollArea, Slider, Ui, ViewportBuilder, ViewportClass,
    ViewportId,
};
use mh_core::{FpsAvailability, SensorStatus, Snapshot};

use crate::controls::parse_hotkey;
use crate::settings::{Metric, SCALES, Settings, TargetChoice};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Overlay,
    Rows,
    Fps,
    Hotkeys,
    About,
}

impl Page {
    const ALL: [Page; 5] = [Page::Overlay, Page::Rows, Page::Fps, Page::Hotkeys, Page::About];

    fn title(self) -> &'static str {
        match self {
            Page::Overlay => "Оверлей",
            Page::Rows => "Строки",
            Page::Fps => "FPS",
            Page::Hotkeys => "Хоткеи",
            Page::About => "О программе",
        }
    }
}

/// Текстовые поля до применения.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Drafts {
    pub manual_process: String,
    pub manual_pid: String,
    pub presentmon_path: String,
    pub toggle_visibility: String,
    pub toggle_lock: String,
}

impl Drafts {
    pub fn from_settings(settings: &Settings) -> Drafts {
        Drafts {
            manual_process: settings.fps.manual_process.clone().unwrap_or_default(),
            manual_pid: settings.fps.manual_pid.map(|pid| pid.to_string()).unwrap_or_default(),
            presentmon_path: settings.fps.presentmon_path.clone().unwrap_or_default(),
            toggle_visibility: settings.hotkeys.toggle_visibility.clone(),
            toggle_lock: settings.hotkeys.toggle_lock.clone(),
        }
    }

    /// Цель: имя и PID. Ошибка — ничего не меняется.
    pub fn apply_target(&self, settings: &mut Settings) -> Result<(), String> {
        let pid = match self.manual_pid.trim() {
            "" => None,
            text => match text.parse::<u32>() {
                Ok(pid) if pid > 0 => Some(pid),
                _ => return Err(format!("PID — положительное целое число, а не «{text}»")),
            },
        };
        let name = self.manual_process.trim();
        settings.fps.manual_process = (!name.is_empty()).then(|| name.to_string());
        settings.fps.manual_pid = pid;
        Ok(())
    }

    /// Свой PresentMon. Пусто — вложенный.
    pub fn apply_presentmon(&self, settings: &mut Settings) -> Result<(), String> {
        let path = self.presentmon_path.trim().trim_matches('"');
        if path.is_empty() {
            settings.fps.presentmon_path = None;
            return Ok(());
        }
        if !Path::new(path).is_file() {
            return Err(format!("файл не найден: {path}"));
        }
        settings.fps.presentmon_path = Some(path.to_string());
        Ok(())
    }

    pub fn apply_hotkeys(&self, settings: &mut Settings) -> Result<(), String> {
        let visibility = self.toggle_visibility.trim();
        let lock = self.toggle_lock.trim();
        for text in [visibility, lock] {
            if parse_hotkey(text).is_none() {
                return Err(format!("не разобрано сочетание «{text}»; пример: Ctrl+Shift+F11"));
            }
        }
        if visibility.eq_ignore_ascii_case(lock) {
            return Err("у двух действий одно сочетание".to_string());
        }
        settings.hotkeys.toggle_visibility = visibility.to_string();
        settings.hotkeys.toggle_lock = lock.to_string();
        Ok(())
    }
}

/// Что окно показывает, но не меняет.
pub struct Context<'a> {
    pub snapshot: &'a Snapshot,
    pub settings_path: &'a Path,
    pub hotkey_errors: &'a [String],
}

#[derive(Default)]
pub struct SettingsWindow {
    pub open: bool,
    page: Page,
    drafts: Drafts,
    /// Итог последнего «Применить» по страницам: (страница, текст, ошибка ли).
    status: Option<(Page, String, bool)>,
    /// Изменилось то, что действует только после перезапуска.
    restart_needed: bool,
    /// Первый кадр окна уже отмечен в журнале.
    first_frame_logged: bool,
    /// Где открыть окно — рядом с HUD, а не под ним: HUD висит поверх всех окон.
    position: Option<Pos2>,
}

/// Размер окна настроек в точках.
pub const SIZE: [f32; 2] = [640.0, 500.0];

impl SettingsWindow {
    pub fn open(&mut self, settings: &Settings, position: Option<Pos2>) {
        if !self.open {
            self.drafts = Drafts::from_settings(settings);
            self.status = None;
            self.position = position;
            self.first_frame_logged = false;
            crate::diag::log("настройки: открытие");
        }
        self.open = true;
    }

    /// Рисует окно, если оно открыто. Вызывать из `ui` корневого окна каждый кадр.
    pub fn show(&mut self, ctx: &egui::Context, settings: &mut Settings, info: &Context<'_>) {
        if !self.open {
            return;
        }
        let mut builder = ViewportBuilder::default()
            .with_title("MH Monitor — настройки")
            .with_inner_size(SIZE)
            .with_min_inner_size([480.0, 360.0]);
        if let Some(position) = self.position {
            builder = builder.with_position(position);
        }
        ctx.show_viewport_immediate(ViewportId::from_hash_of("settings"), builder, |ui, class| {
            if class == ViewportClass::EmbeddedWindow {
                // Встроенных окон нет у нативного eframe; ветка на случай иной сборки.
                ui.label("окно настроек недоступно");
                return;
            }
            if !self.first_frame_logged {
                self.first_frame_logged = true;
                crate::diag::log("настройки: первый кадр");
            }
            if ui.ctx().input(|input| input.viewport().close_requested()) {
                self.open = false;
            }
            egui::Panel::left("settings-pages").resizable(false).exact_size(150.0).show(ui, |ui| {
                ui.add_space(10.0);
                for page in Page::ALL {
                    let text = RichText::new(page.title()).font(theme::regular(15.0));
                    if ui.selectable_label(self.page == page, text).clicked() {
                        self.page = page;
                    }
                }
            });
            egui::CentralPanel::default().show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| match self.page {
                    Page::Overlay => overlay_page(ui, settings),
                    Page::Rows => rows_page(ui, settings),
                    Page::Fps => self.fps_page(ui, settings, info),
                    Page::Hotkeys => self.hotkeys_page(ui, settings, info),
                    Page::About => about_page(ui, info),
                });
            });
        });
    }

    fn fps_page(&mut self, ui: &mut Ui, settings: &mut Settings, info: &Context<'_>) {
        group(ui, "Сейчас");
        let state = &info.snapshot.fps;
        let target = state
            .target
            .as_ref()
            .map(|target| format!("{} (pid {})", target.executable, target.pid))
            .unwrap_or_else(|| "—".to_string());
        note(ui, &format!("Цель: {target}"));
        note(ui, &format!("Состояние: {}", availability_text(state.availability)));
        if let Some(presentation) = &state.presentation {
            note(ui, &format!("Вывод: {presentation}"));
        }
        if let Some(message) = state.message() {
            note(ui, message);
        }
        if let Some(detail) = &state.detail {
            // Сырые слова источника — здесь, в диагностике, а не в HUD.
            hint(ui, detail);
        }
        hint(ui, "Для захвата кадров нужны права администратора.");

        group(ui, "Цель");
        let fps = &mut settings.fps;
        ui.radio_value(&mut fps.target, TargetChoice::Auto, "Окно в фокусе");
        ui.radio_value(&mut fps.target, TargetChoice::Manual, "Выбранный процесс");
        if fps.target == TargetChoice::Manual {
            text_field(ui, "Имя процесса", &mut self.drafts.manual_process, "game.exe");
            text_field(
                ui,
                "PID (необязательно)",
                &mut self.drafts.manual_pid,
                "если копий несколько",
            );
            if apply_button(ui) {
                let result = self.drafts.apply_target(settings);
                self.report(Page::Fps, result, "цель применена");
            }
        }

        group(ui, "PresentMon");
        text_field(
            ui,
            "Свой PresentMon.exe",
            &mut self.drafts.presentmon_path,
            "пусто — вложенный 2.5.1",
        );
        if apply_button(ui) {
            let before = settings.fps.presentmon_path.clone();
            let result = self.drafts.apply_presentmon(settings);
            if result.is_ok() && settings.fps.presentmon_path != before {
                self.restart_needed = true;
            }
            self.report(Page::Fps, result, "путь сохранён");
        }
        self.status_line(ui, Page::Fps);
    }

    fn hotkeys_page(&mut self, ui: &mut Ui, settings: &mut Settings, info: &Context<'_>) {
        group(ui, "Глобальные сочетания");
        text_field(ui, "Показать / скрыть", &mut self.drafts.toggle_visibility, "Ctrl+Shift+F11");
        text_field(ui, "Заблокировать", &mut self.drafts.toggle_lock, "Ctrl+Shift+F10");
        if apply_button(ui) {
            let before = settings.hotkeys.clone();
            let result = self.drafts.apply_hotkeys(settings);
            if result.is_ok() && settings.hotkeys != before {
                self.restart_needed = true;
            }
            self.report(Page::Hotkeys, result, "сочетания сохранены");
        }
        self.status_line(ui, Page::Hotkeys);
        for error in info.hotkey_errors {
            warning(ui, error);
        }
    }

    fn report(&mut self, page: Page, result: Result<(), String>, ok: &str) {
        self.status = Some(match result {
            Ok(()) => (page, ok.to_string(), false),
            Err(error) => (page, error, true),
        });
    }

    fn status_line(&self, ui: &mut Ui, page: Page) {
        if let Some((status_page, text, is_error)) = &self.status
            && *status_page == page
        {
            ui.add_space(6.0);
            if *is_error {
                warning(ui, text);
            } else {
                note(ui, text);
            }
        }
        if self.restart_needed {
            ui.add_space(6.0);
            warning(ui, "Часть изменений вступит в силу после перезапуска MH Monitor.");
        }
    }
}

fn overlay_page(ui: &mut Ui, settings: &mut Settings) {
    let overlay = &mut settings.overlay;
    group(ui, "Окно");
    ui.add(
        Slider::new(&mut overlay.opacity, crate::settings::MIN_OPACITY..=1.0)
            .custom_formatter(|value, _| format!("{:.0}%", value * 100.0))
            .custom_parser(|text| {
                text.trim_end_matches('%').trim().parse::<f64>().ok().map(|v| v / 100.0)
            })
            .text("непрозрачность фона"),
    );
    hint(ui, "Текст всегда непрозрачный — бледнеет только фон.");
    ui.horizontal(|ui| {
        ui.label("Масштаб:");
        for scale in SCALES {
            ui.selectable_value(&mut overlay.scale, scale, format!("{:.0}%", scale * 100.0));
        }
    });
    hint(ui, "Масштаб действует и на это окно.");
    ui.checkbox(&mut overlay.always_on_top, "Поверх всех окон");
    ui.checkbox(&mut overlay.locked, "Заблокирован: мышь проходит сквозь HUD");
    ui.checkbox(&mut overlay.show_header, "Заголовок с кнопками");

    group(ui, "График");
    ui.checkbox(&mut overlay.show_graph, "График frametime");
}

fn rows_page(ui: &mut Ui, settings: &mut Settings) {
    group(ui, "Секции");
    let sections = &mut settings.sections;
    ui.horizontal(|ui| {
        ui.checkbox(&mut sections.gpu, "GPU");
        ui.checkbox(&mut sections.cpu, "CPU");
        ui.checkbox(&mut sections.ram, "RAM");
        ui.checkbox(&mut sections.fps, "FPS");
    });
    ui.checkbox(
        &mut settings.metrics.hide_unavailable,
        "Скрывать строки, которые эта машина не сообщает",
    );
    hint(ui, "Строка, хоть раз показавшая значение, место не теряет.");

    let groups: [(&str, &[(Metric, &str)]); 4] = [
        (
            "GPU",
            &[
                (Metric::GpuTemperature, "Температура"),
                (Metric::GpuClock, "Частота ядра"),
                (Metric::GpuVram, "Видеопамять"),
                (Metric::GpuPower, "Мощность"),
                (Metric::GpuFan, "Вентилятор"),
            ],
        ),
        (
            "CPU",
            &[
                (Metric::CpuTemperature, "Температура"),
                (Metric::CpuClock, "Частота"),
                (Metric::CpuPower, "Мощность"),
            ],
        ),
        ("RAM", &[(Metric::RamUsed, "Занято / всего")]),
        (
            "FPS",
            &[
                (Metric::FpsAverage, "Средний"),
                (Metric::FpsLow1, "1% low"),
                (Metric::FpsLow01, "0.1% low"),
            ],
        ),
    ];
    for (title, metrics) in groups {
        group(ui, title);
        for (metric, label) in metrics {
            let mut enabled = settings.metrics.is_enabled(*metric);
            if ui.checkbox(&mut enabled, *label).changed() {
                settings.metrics.set_enabled(*metric, enabled);
            }
        }
    }
}

fn about_page(ui: &mut Ui, info: &Context<'_>) {
    group(ui, "MH Monitor");
    note(ui, &format!("Версия {}", env!("CARGO_PKG_VERSION")));
    note(ui, &format!("Настройки: {}", info.settings_path.display()));
    hint(ui, "PresentMon 2.5.1 (MIT) и модуль PawnIO AMDFamily17 (LGPL-2.1) встроены.");
    hint(ui, "Шрифт Cuprum — SIL Open Font License 1.1.");
    let snapshot = info.snapshot;
    group(ui, "Датчики");
    note(ui, &format!("Железо: {}", status_text(snapshot.hardware_status)));
    for (name, health) in [
        ("GPU", snapshot.gpu.health),
        ("CPU", snapshot.cpu.health),
        ("RAM", snapshot.memory.health),
    ] {
        let reason = health.reason.map(|r| format!(" — {r}")).unwrap_or_default();
        note(ui, &format!("{name}: {}{reason}", status_text(health.status)));
    }
}

fn availability_text(availability: FpsAvailability) -> &'static str {
    match availability {
        FpsAvailability::Unknown => "определяется",
        FpsAvailability::Waiting => "ожидание",
        FpsAvailability::Available => "замер идёт",
        FpsAvailability::Unavailable => "недоступно",
        FpsAvailability::Error => "ошибка",
    }
}

fn status_text(status: SensorStatus) -> &'static str {
    match status {
        SensorStatus::Unknown => "определяется",
        SensorStatus::Available => "всё читается",
        SensorStatus::Partial => "частично",
        SensorStatus::Unavailable => "недоступно",
        SensorStatus::Error => "ошибка",
    }
}

fn group(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).font(theme::bold(16.0)).color(theme::TEXT_PRIMARY));
    ui.add_space(4.0);
}

fn note(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::TEXT_SECONDARY));
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::regular(12.0)).color(theme::TEXT_DISABLED));
}

fn warning(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::ACCENT_WARN));
}

fn text_field(ui: &mut Ui, label: &str, value: &mut String, placeholder: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(170.0, 20.0), egui::Sense::hover());
        ui.put(rect, egui::Label::new(label).halign(egui::Align::LEFT));
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(RichText::new(placeholder).color(Color32::GRAY))
                .desired_width(280.0),
        );
    });
}

fn apply_button(ui: &mut Ui) -> bool {
    ui.add_space(4.0);
    let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
    ui.button("Применить").clicked() || enter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_round_trip_the_settings() {
        let mut settings = Settings::default();
        settings.fps.manual_process = Some("dmc4.exe".into());
        settings.fps.manual_pid = Some(42);
        let drafts = Drafts::from_settings(&settings);
        assert_eq!(drafts.manual_pid, "42");
        let mut copy = Settings::default();
        drafts.apply_target(&mut copy).unwrap();
        assert_eq!(copy.fps.manual_process.as_deref(), Some("dmc4.exe"));
        assert_eq!(copy.fps.manual_pid, Some(42));
    }

    #[test]
    fn a_bad_pid_changes_nothing() {
        let mut settings = Settings::default();
        settings.fps.manual_process = Some("old.exe".into());
        let drafts = Drafts {
            manual_process: "new.exe".into(),
            manual_pid: "-3".into(),
            ..Default::default()
        };
        assert!(drafts.apply_target(&mut settings).unwrap_err().contains("-3"));
        assert_eq!(settings.fps.manual_process.as_deref(), Some("old.exe"));
        let zero = Drafts { manual_pid: "0".into(), ..Default::default() };
        assert!(zero.apply_target(&mut settings).is_err());
    }

    #[test]
    fn blank_fields_clear_the_target() {
        let mut settings = Settings::default();
        settings.fps.manual_process = Some("old.exe".into());
        let drafts = Drafts { manual_process: "  ".into(), ..Default::default() };
        drafts.apply_target(&mut settings).unwrap();
        assert_eq!(settings.fps.manual_process, None);
        assert_eq!(settings.fps.manual_pid, None);
    }

    #[test]
    fn a_missing_presentmon_is_rejected_and_blank_means_bundled() {
        let mut settings = Settings::default();
        let missing =
            Drafts { presentmon_path: r"C:\no\such\PresentMon.exe".into(), ..Default::default() };
        assert!(missing.apply_presentmon(&mut settings).is_err());
        assert_eq!(settings.fps.presentmon_path, None);

        let this_exe = std::env::current_exe().unwrap();
        let quoted =
            Drafts { presentmon_path: format!("\"{}\"", this_exe.display()), ..Default::default() };
        quoted.apply_presentmon(&mut settings).unwrap();
        assert_eq!(settings.fps.presentmon_path.as_deref(), this_exe.to_str());

        Drafts::default().apply_presentmon(&mut settings).unwrap();
        assert_eq!(settings.fps.presentmon_path, None);
    }

    #[test]
    fn hotkeys_are_validated_before_saving() {
        let mut settings = Settings::default();
        let bad = Drafts {
            toggle_visibility: "Ctrl+Nonsense".into(),
            toggle_lock: "Ctrl+Shift+F10".into(),
            ..Default::default()
        };
        assert!(bad.apply_hotkeys(&mut settings).is_err());
        let same = Drafts {
            toggle_visibility: "Ctrl+Shift+F9".into(),
            toggle_lock: "ctrl+shift+f9".into(),
            ..Default::default()
        };
        assert!(same.apply_hotkeys(&mut settings).is_err());
        let good = Drafts {
            toggle_visibility: "Alt+F9".into(),
            toggle_lock: "Alt+F8".into(),
            ..Default::default()
        };
        good.apply_hotkeys(&mut settings).unwrap();
        assert_eq!(settings.hotkeys.toggle_lock, "Alt+F8");
    }
}

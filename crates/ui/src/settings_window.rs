//! Окно настроек (PLAN.md §6/P5).
//!
//! Три колонки: разделы, содержимое раздела, предпросмотр HUD. Всё меняется в черновике —
//! предпросмотр показывает его сразу, а настоящий HUD меняется только кнопкой «Применить».
//! «Отмена» и закрытие окна черновик выбрасывают. Исключение — действия, которые настройками не
//! являются: установка, автозапуск, запуск игры и ярлык выполняются сразу.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, Pos2, RichText, ScrollArea, Sense, Stroke,
    Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, WindowLevel, vec2,
};
use mh_core::{FpsAvailability, SensorStatus, Snapshot};
use mh_platform::dialog::FileFilter;

use crate::blocks::{
    Block, BlockKind, LOAD_CRITICAL_PERCENT, LOAD_WARN_PERCENT, MAX_TEMPERATURE, MAX_TITLE_CHARS,
    MIN_TEMPERATURE,
};
use crate::controls::parse_hotkey;
use crate::placement::{Corner, MAX_OFFSET};
use crate::settings::{
    FullscreenMode, MAX_SCALE, MAX_STRIP_HEIGHT, MAX_WIDTH_SCALE, MIN_OPACITY, MIN_SCALE,
    MIN_STRIP_HEIGHT, MIN_WIDTH_SCALE, Metric, OverlayMode, SCALE_STEP, Settings, TargetChoice,
};
use crate::units::{Frequency, Memory, Temperature};
use crate::widgets::{self, Unit, card, page_title, row, switch_row, switch_row_enabled};
use crate::{format, preview, theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    General,
    Fps,
    Gpu,
    Cpu,
    Memory,
    Games,
    Hotkeys,
    Service,
    About,
}

impl Page {
    const ALL: [Page; 9] = [
        Page::General,
        Page::Fps,
        Page::Gpu,
        Page::Cpu,
        Page::Memory,
        Page::Games,
        Page::Hotkeys,
        Page::Service,
        Page::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::General => "Общие",
            Page::Fps => "FPS",
            Page::Gpu => "Видеокарта",
            Page::Cpu => "Процессор",
            Page::Memory => "ОЗУ",
            Page::Games => "Игры",
            Page::Hotkeys => "Горячие клавиши",
            Page::Service => "Установка",
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
    /// Путь к игре, которую добавляют.
    pub game_path: String,
}

impl Drafts {
    pub fn from_settings(settings: &Settings) -> Drafts {
        Drafts {
            manual_process: settings.fps.manual_process.clone().unwrap_or_default(),
            manual_pid: settings.fps.manual_pid.map(|pid| pid.to_string()).unwrap_or_default(),
            presentmon_path: settings.fps.presentmon_path.clone().unwrap_or_default(),
            toggle_visibility: settings.hotkeys.toggle_visibility.clone(),
            toggle_lock: settings.hotkeys.toggle_lock.clone(),
            game_path: String::new(),
        }
    }

    /// Все поля разом. Первая ошибка — ничего не меняется.
    pub fn apply_all(&self, settings: &mut Settings) -> Result<(), String> {
        let mut next = settings.clone();
        self.apply_target(&mut next)?;
        self.apply_presentmon(&mut next)?;
        self.apply_hotkeys(&mut next)?;
        *settings = next;
        Ok(())
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
            return Err(format!("файл PresentMon не найден: {path}"));
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

    /// Поля без добавляемой игры: она к настройкам не относится, пока её не добавили.
    fn settings_part(&self) -> Drafts {
        Drafts { game_path: String::new(), ..self.clone() }
    }
}

/// Черновик настроек и то, от чего он отсчитан.
///
/// `base` — рабочие настройки на момент последней сверки. Пока пользователь чего-то не трогал,
/// черновик идёт за рабочими настройками: HUD перетащили, заблокировали из трея — окно это видит.
#[derive(Debug, Clone, Default)]
struct Draft {
    base: Settings,
    settings: Settings,
    fields: Drafts,
}

impl Draft {
    fn of(live: &Settings) -> Draft {
        Draft { base: live.clone(), settings: live.clone(), fields: Drafts::from_settings(live) }
    }

    fn is_dirty(&self) -> bool {
        self.settings != self.base
            || self.fields.settings_part() != Drafts::from_settings(&self.base)
    }

    /// Сверка с рабочими настройками, которые могли поменяться мимо окна.
    fn follow(&mut self, live: &Settings) {
        if self.base == *live {
            return;
        }
        let old = std::mem::replace(&mut self.base, live.clone());
        if self.settings == old {
            self.settings = live.clone();
        } else {
            // Правки пользователя остаются; то, что меняется мимо окна, идёт за рабочими.
            let draft = &mut self.settings;
            draft.overlay.x = live.overlay.x;
            draft.overlay.y = live.overlay.y;
            draft.setup_done = live.setup_done;
            if draft.overlay.visible == old.overlay.visible {
                draft.overlay.visible = live.overlay.visible;
            }
            if draft.overlay.locked == old.overlay.locked {
                draft.overlay.locked = live.overlay.locked;
            }
        }
        if Drafts::from_settings(&old) == self.fields.settings_part() {
            let game_path = std::mem::take(&mut self.fields.game_path);
            self.fields = Drafts { game_path, ..Drafts::from_settings(live) };
        }
    }

    /// Настройки, которые получатся после «Применить».
    fn result(&self) -> Result<Settings, String> {
        let mut next = self.settings.clone();
        self.fields.apply_all(&mut next)?;
        Ok(next.sanitized())
    }

    /// Заменяет черновик целиком, кроме того, что относится к этой машине: позиции HUD и
    /// пройденного первого запуска. Список игр «Сбросить» передаёт сам.
    fn replace(&mut self, mut incoming: Settings) {
        let current = &self.settings;
        incoming.overlay.x = current.overlay.x;
        incoming.overlay.y = current.overlay.y;
        incoming.setup_done = current.setup_done;
        let game_path = std::mem::take(&mut self.fields.game_path);
        self.fields = Drafts { game_path, ..Drafts::from_settings(&incoming) };
        self.settings = incoming;
    }
}

/// Что пользователь попросил сделать с установкой. Выполняет приложение: нужен UAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceRequest {
    /// Установить или обновить копию в Program Files вместе со службой.
    Install,
    Uninstall,
    /// Запустить остановленную службу.
    StartService,
}

/// Что окно показывает, но не меняет.
pub struct Context<'a> {
    pub snapshot: &'a Snapshot,
    pub settings_path: &'a Path,
    pub hotkey_errors: &'a [String],
    /// Откуда снимки: служба или свой движок.
    pub backend: &'a str,
    /// Итог последней операции со службой.
    pub service_status: Option<&'a str>,
    /// Операция со службой ещё идёт.
    pub service_busy: bool,
    /// Мониторы для закрепления HUD: первым — «основной», дальше по номерам.
    pub monitors: &'a [String],
    /// Движок примет интервал опроса. Нет — служба старой версии.
    pub sensor_options_supported: bool,
}

/// Что известно об установке.
#[derive(Clone)]
struct InstallState {
    installed_version: Option<String>,
    running_installed: bool,
    autostart: bool,
    service_exe: Option<std::path::PathBuf>,
    service_running: bool,
    checked: std::time::Instant,
}

impl InstallState {
    fn read() -> InstallState {
        InstallState {
            installed_version: crate::installer::installed_version(),
            running_installed: crate::installer::running_installed(),
            autostart: crate::installer::autostart_enabled(),
            service_exe: crate::service::registered_executable(),
            service_running: crate::service::is_running(),
            checked: std::time::Instant::now(),
        }
    }
}

/// Зачем открыт системный диалог файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picking {
    Game,
    Export,
    Import,
}

const SETTINGS_FILTER: FileFilter<'static> =
    FileFilter { name: "Настройки MH Monitoring (*.json)", pattern: "*.json" };

#[derive(Default)]
pub struct SettingsWindow {
    pub open: bool,
    page: Page,
    draft: Draft,
    /// Итог последнего действия: текст и ошибка ли.
    status: Option<(String, bool)>,
    /// Применено то, что действует только после перезапуска.
    restart_needed: bool,
    /// Первый кадр окна уже отмечен в журнале.
    first_frame_logged: bool,
    /// Состояние установки — с отметкой, когда проверено: спрашивать систему на каждом кадре
    /// незачем.
    install_state: Option<InstallState>,
    /// Где открыть окно — рядом с HUD, а не под ним: HUD висит поверх всех окон.
    position: Option<Pos2>,
    /// Вывести окно вперёд на ближайшем кадре: открыли из трея или хоткеем поверх игры.
    focus_pending: bool,
    /// Открыт диалог выбора файла; ответ придёт сюда.
    picking: Option<(Picking, Receiver<Option<PathBuf>>)>,
    /// Цифры для предпросмотра — одни на всё время жизни окна.
    demo: Option<Snapshot>,
    /// Видеокарты для выбора; читаются при открытии окна.
    gpus: Vec<String>,
}

/// Размер окна настроек в точках.
pub const SIZE: [f32; 2] = [1080.0, 720.0];
const NAV_WIDTH: f32 = 200.0;
/// Самая узкая панель предпросмотра; шире HUD она становится вместе с ним.
const PREVIEW_MIN_WIDTH: f32 = 320.0;
const PREVIEW_MARGIN: f32 = 56.0;

impl SettingsWindow {
    pub fn open(&mut self, settings: &Settings, position: Option<Pos2>) {
        if !self.open {
            self.draft = Draft::of(settings);
            self.gpus = gpu_names();
            self.status = None;
            self.position = position;
            self.first_frame_logged = false;
            crate::diag::log("настройки: открытие");
        }
        self.open = true;
        self.focus_pending = true;
    }

    /// Открывает окно сразу на нужном разделе — например, на «Установке» при первом запуске.
    pub fn open_page(&mut self, settings: &Settings, position: Option<Pos2>, page: Page) {
        self.open(settings, position);
        self.page = page;
    }

    /// Рисует окно, если оно открыто. Вызывать из `ui` корневого окна каждый кадр.
    /// «Применить» пишет в `settings`. Возвращает просьбу об операции со службой.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        settings: &mut Settings,
        info: &Context<'_>,
    ) -> Option<ServiceRequest> {
        if !self.open {
            return None;
        }
        self.draft.follow(settings);
        self.poll_picker();

        let mut builder = ViewportBuilder::default()
            .with_title("MH Monitoring — настройки")
            .with_inner_size(SIZE)
            .with_min_inner_size([860.0, 520.0])
            // Поверх всех окон, как и HUD: иначе безрамочная игра закрывает настройки, и найти их
            // можно только через панель задач.
            .with_window_level(WindowLevel::AlwaysOnTop);
        if let Some(position) = self.position {
            builder = builder.with_position(position);
        }
        ctx.show_viewport_immediate(ViewportId::from_hash_of("settings"), builder, |ui, class| {
            if class == ViewportClass::EmbeddedWindow {
                // Встроенных окон нет у нативного eframe; ветка на случай иной сборки.
                ui.label("окно настроек недоступно");
                return None;
            }
            if !self.first_frame_logged {
                self.first_frame_logged = true;
                crate::diag::log("настройки: первый кадр");
            }
            if std::mem::take(&mut self.focus_pending) {
                ui.ctx().send_viewport_cmd(ViewportCommand::Focus);
            }
            if ui.ctx().input(|input| input.viewport().close_requested()) {
                // Закрыли крестиком — как «Отмена».
                self.open = false;
            }
            style_window(ui);

            egui::Panel::bottom("settings-footer")
                .resizable(false)
                .exact_size(56.0)
                .frame(panel_frame(theme::WINDOW_PANEL, Margin::symmetric(20, 12)))
                .show(ui, |ui| self.footer(ui, settings));
            egui::Panel::left("settings-pages")
                .resizable(false)
                .exact_size(NAV_WIDTH)
                .frame(panel_frame(theme::WINDOW_PANEL, Margin::symmetric(12, 14)))
                .show(ui, |ui| self.navigation(ui));
            egui::Panel::right("settings-preview")
                .resizable(false)
                .exact_size(
                    (crate::hud::width(&self.draft.settings) + PREVIEW_MARGIN)
                        .max(PREVIEW_MIN_WIDTH),
                )
                .frame(panel_frame(theme::WINDOW_PANEL, Margin::symmetric(16, 14)))
                .show(ui, |ui| self.preview(ui, info.snapshot));

            let mut request = None;
            egui::CentralPanel::default()
                .frame(panel_frame(theme::WINDOW_BACKGROUND, Margin::symmetric(20, 14)))
                .show(ui, |ui| {
                    // Без auto_shrink: иначе область сжимается по содержимому, и полоса
                    // прокрутки стоит посреди окна, а не у правого края.
                    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_max_width(ui.available_width() - 12.0);
                        page_title(ui, self.page.title());
                        match self.page {
                            Page::General => general_page(ui, &mut self.draft.settings, info),
                            Page::Fps => fps_page(ui, &mut self.draft, info),
                            Page::Gpu => gpu_page(ui, &mut self.draft.settings, info, &self.gpus),
                            Page::Cpu => cpu_page(ui, &mut self.draft.settings, info),
                            Page::Memory => memory_page(ui, &mut self.draft.settings),
                            Page::Games => self.games_page(ui),
                            Page::Hotkeys => hotkeys_page(ui, &mut self.draft.fields, info),
                            Page::Service => request = self.service_page(ui, info),
                            Page::About => about_page(ui, info),
                        }
                    });
                });
            request
        })
    }

    fn navigation(&mut self, ui: &mut Ui) {
        for page in Page::ALL {
            if nav_item(ui, page.title(), self.page == page).clicked() {
                self.page = page;
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            let busy = self.picking.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                if side_button(ui, "Сбросить настройки").clicked() {
                    self.draft.replace(Settings {
                        games: self.draft.settings.games.clone(),
                        ..Settings::default()
                    });
                    self.set_status(Ok("значения по умолчанию — нажмите «Применить»".into()));
                }
                if side_button(ui, "Загрузить из файла…").clicked() {
                    self.pick(ui, Picking::Import);
                }
                if side_button(ui, "Сохранить в файл…").clicked() {
                    self.pick(ui, Picking::Export);
                }
            });
        });
    }

    fn preview(&mut self, ui: &mut Ui, live: &Snapshot) {
        ui.label(
            RichText::new("Предпросмотр оверлея")
                .font(theme::bold(15.0))
                .color(theme::TEXT_PRIMARY),
        );
        ui.add_space(10.0);
        let snapshot = self.demo.get_or_insert_with(preview::demo_snapshot);
        preview::use_live_devices(snapshot, live);
        let settings = &self.draft.settings;
        // Со всеми строками HUD выше окна — прокручивается вместе с пояснением.
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgb(0x05, 0x06, 0x08))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // По центру отступом, а не центрирующей раскладкой: та выровняла бы по
                    // центру и подписи внутри HUD, а в настоящем оверлее они слева.
                    // Полоса шире панели — она одна прокручивается вбок.
                    ScrollArea::horizontal().id_salt("preview-strip").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let free = ui.available_width() - crate::hud::width(settings);
                            if settings.overlay.mode == OverlayMode::Full {
                                ui.add_space((free / 2.0).max(0.0));
                            }
                            ui.vertical(|ui| preview::show(ui, snapshot, settings));
                        });
                    });
                });
            ui.add_space(10.0);
            widgets::note_box(
                ui,
                "Предпросмотр меняется сразу, на выдуманных цифрах. Настоящий оверлей — после \
                 «Применить». Масштаб в предпросмотре не показывается.",
            );
        });
    }

    fn footer(&mut self, ui: &mut Ui, settings: &mut Settings) {
        ui.horizontal_centered(|ui| {
            match &self.status {
                Some((text, true)) => {
                    ui.label(RichText::new(text).color(theme::ACCENT_WARN));
                }
                Some((text, false)) => {
                    ui.label(RichText::new(text).color(theme::TEXT_SECONDARY));
                }
                None if self.draft.is_dirty() => {
                    ui.label(RichText::new("Есть неприменённые изменения").color(theme::ACCENT));
                }
                None => {}
            }
            if self.restart_needed {
                ui.label(
                    RichText::new("· часть изменений — после перезапуска")
                        .color(theme::ACCENT_WARN),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::button(ui, "Отмена", false).clicked() {
                    self.draft = Draft::of(settings);
                    self.status = None;
                    self.open = false;
                }
                ui.add_space(8.0);
                let dirty = self.draft.is_dirty();
                if ui
                    .add_enabled_ui(dirty, |ui| widgets::button(ui, "Применить", true))
                    .inner
                    .clicked()
                {
                    self.apply(settings);
                }
            });
        });
    }

    fn apply(&mut self, settings: &mut Settings) {
        let next = match self.draft.result() {
            Ok(next) => next,
            Err(error) => {
                self.set_status(Err(error));
                return;
            }
        };
        if next.fps.presentmon_path != settings.fps.presentmon_path
            || next.hotkeys != settings.hotkeys
        {
            self.restart_needed = true;
        }
        *settings = next;
        self.draft = Draft::of(settings);
        self.set_status(Ok("применено".into()));
    }

    fn set_status(&mut self, result: Result<String, String>) {
        self.status = Some(match result {
            Ok(text) => (text, false),
            Err(error) => (error, true),
        });
    }

    /// Открывает системный диалог в своём потоке: он блокирует.
    fn pick(&mut self, ui: &Ui, purpose: Picking) {
        let (sender, receiver) = mpsc::channel();
        let repaint = ui.ctx().clone();
        let spawned = std::thread::Builder::new().name("mh-pick-file".into()).spawn(move || {
            let picked = match purpose {
                Picking::Game => mh_platform::dialog::pick_executable("Файл игры"),
                Picking::Import => {
                    mh_platform::dialog::pick_file("Загрузить настройки", SETTINGS_FILTER)
                }
                Picking::Export => mh_platform::dialog::pick_save_path(
                    "Сохранить настройки",
                    "MH-Monitoring-settings.json",
                    SETTINGS_FILTER,
                ),
            };
            let _ = sender.send(picked);
            repaint.request_repaint_of(ViewportId::ROOT);
        });
        if spawned.is_ok() {
            self.picking = Some((purpose, receiver));
        }
    }

    fn poll_picker(&mut self) {
        let Some((purpose, receiver)) = &self.picking else { return };
        let purpose = *purpose;
        let path = match receiver.try_recv() {
            Ok(path) => path,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        self.picking = None;
        let Some(path) = path else { return };
        match purpose {
            Picking::Game => {
                if self.add_game(&path) {
                    self.draft.fields.game_path.clear();
                }
            }
            Picking::Export => {
                let result = match self.draft.result() {
                    Ok(settings) => settings
                        .save(&path)
                        .map(|()| format!("сохранено: {}", path.display()))
                        .map_err(|error| format!("не сохранено: {error}")),
                    Err(error) => Err(error),
                };
                self.set_status(result);
            }
            Picking::Import => match Settings::import(&path) {
                Ok(loaded) => {
                    self.draft.replace(loaded.settings);
                    let mut text = "загружено — нажмите «Применить»".to_string();
                    if let Some(notice) = loaded.notice {
                        text.push_str("; ");
                        text.push_str(&notice);
                    }
                    self.set_status(Ok(text));
                }
                Err(error) => self.set_status(Err(error)),
            },
        }
    }

    fn service_page(&mut self, ui: &mut Ui, info: &Context<'_>) -> Option<ServiceRequest> {
        let fresh = self
            .install_state
            .as_ref()
            .is_some_and(|state| state.checked.elapsed() < std::time::Duration::from_secs(2));
        if !fresh && !info.service_busy {
            self.install_state = Some(InstallState::read());
        }
        let state = self.install_state.clone()?;
        let mut request = None;
        hint(ui, "Действия на этой странице выполняются сразу, без «Применить».");
        ui.add_space(6.0);

        card(ui, "MH Monitoring", |ui| {
            match &state.installed_version {
                Some(version) => note(ui, &format!("Установлен, версия {version}.")),
                None => note(ui, "Не установлен."),
            }
            let this_version = env!("CARGO_PKG_VERSION");
            if !state.running_installed {
                hint(ui, &format!("Запущена копия {this_version} не из папки установки."));
            }
            hint(
                ui,
                &format!(
                    "Установка копирует программу в {} и ставит службу (отдельный \
                     MH-Monitoring-Service.exe): захват кадров и температура CPU работают без \
                     прав администратора. Подтверждение UAC спросят один раз.",
                    crate::installer::install_dir().display()
                ),
            );
            ui.add_space(6.0);
            ui.add_enabled_ui(!info.service_busy, |ui| {
                ui.horizontal(|ui| {
                    let label = match (&state.installed_version, state.running_installed) {
                        (None, _) => Some("Установить"),
                        (Some(_), false) => Some("Обновить установленную"),
                        (Some(_), true) => None,
                    };
                    if let Some(label) = label
                        && widgets::button(ui, label, true).clicked()
                    {
                        request = Some(ServiceRequest::Install);
                    }
                    if state.installed_version.is_some()
                        && widgets::button(ui, "Удалить", false).clicked()
                    {
                        request = Some(ServiceRequest::Uninstall);
                    }
                });
            });
            if let Some(status) = info.service_status {
                ui.add_space(4.0);
                note(ui, &format!("Последняя операция: {status}"));
            }
        });
        if request.is_some() {
            self.install_state = None;
        }

        card(ui, "Запуск", |ui| {
            let mut autostart = state.autostart;
            if switch_row(ui, "Запускать вместе с Windows", None, &mut autostart).changed()
            {
                match crate::installer::set_autostart(autostart) {
                    Ok(()) => {
                        if let Some(state) = self.install_state.as_mut() {
                            state.autostart = autostart;
                        }
                    }
                    Err(error) => self.set_status(Err(format!("автозапуск: {error}"))),
                }
            }
        });

        card(ui, "Служба", |ui| {
            note(ui, &format!("Снимки сейчас: {}", info.backend));
            match &state.service_exe {
                Some(path) => {
                    hint(ui, &format!("Служба запускает: {}", path.display()));
                    if !state.service_running {
                        warning(
                            ui,
                            "Служба остановлена: без неё кадры и температура CPU недоступны.",
                        );
                        if ui
                            .add_enabled_ui(!info.service_busy, |ui| {
                                widgets::button(ui, "Запустить службу", true)
                            })
                            .inner
                            .clicked()
                        {
                            request = Some(ServiceRequest::StartService);
                            self.install_state = None;
                        }
                    }
                    if !crate::installer::same_path(
                        path,
                        &crate::installer::installed_service_exe(),
                    ) {
                        warning(
                            ui,
                            "Служба стоит не на установленной копии. Если этот файл лежит там, \
                             куда пишут обычные пользователи, его подмена даст права системы — \
                             переустановите программу.",
                        );
                    }
                }
                None => hint(ui, "Служба не установлена."),
            }
        });
        request
    }

    fn games_page(&mut self, ui: &mut Ui) {
        card(ui, "Старые игры без безрамочного режима", |ui| {
            hint(
                ui,
                "MH Monitoring запускает игру с параметрами (у Warcraft III 1.26 — -window) и, \
                 если отмечено, снимает с окна игры рамку и растягивает его на весь монитор. HUD \
                 тогда виден поверх игры. В игру ничего не внедряется: меняется только её окно.",
            );
            hint(
                ui,
                "Ярлык на рабочем столе запускает игру вместе с оверлеем. Картинка игры 4:3 на \
                 широком мониторе растягивается. Список игр и их параметры сохраняются кнопкой \
                 «Применить».",
            );
        });

        enum Action {
            Launch(usize),
            Shortcut(usize),
            Remove(usize),
        }
        let mut action = None;
        let games = &mut self.draft.settings.games;
        if games.is_empty() {
            card(ui, "Игры", |ui| note(ui, "Игр пока нет."));
        }
        for (index, game) in games.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                card(ui, &game.name.clone(), |ui| {
                    hint(ui, &game.path);
                    row(ui, "Параметры запуска", None, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut game.arguments)
                                .desired_width(fit(ui, 260.0)),
                        )
                    });
                    let help =
                        game.window_exe.as_ref().map(|w| format!("Рамка снимается с окна {w}."));
                    switch_row(
                        ui,
                        "Без рамки на весь монитор",
                        help.as_deref(),
                        &mut game.borderless,
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if widgets::button(ui, "Запустить", true).clicked() {
                            action = Some(Action::Launch(index));
                        }
                        if widgets::button(ui, "Ярлык на рабочем столе", false).clicked()
                        {
                            action = Some(Action::Shortcut(index));
                        }
                        if widgets::button(ui, "Удалить", false).clicked() {
                            action = Some(Action::Remove(index));
                        }
                    });
                });
            });
        }
        match action {
            Some(Action::Launch(index)) => {
                let result = crate::games::launch(&self.draft.settings.games[index])
                    .map(|()| "игра запущена".to_string());
                self.set_status(result);
            }
            Some(Action::Shortcut(index)) => {
                let result =
                    crate::games::create_desktop_shortcut(&self.draft.settings.games[index])
                        .map(|link| format!("ярлык создан: {}", link.display()));
                self.set_status(result);
            }
            Some(Action::Remove(index)) => {
                let removed = self.draft.settings.games.remove(index);
                self.set_status(Ok(format!("удалено: {} — нажмите «Применить»", removed.name)));
            }
            None => {}
        }

        let picking = self.picking.is_some();
        let mut pick = false;
        let mut add = false;
        card(ui, "Добавить игру", |ui| {
            row(ui, "Файл игры", None, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft.fields.game_path)
                        .hint_text(r"C:\Games\game.exe")
                        .desired_width(fit(ui, 280.0)),
                );
                ui.add_enabled_ui(!picking, |ui| {
                    pick = ui.button("Обзор…").clicked();
                });
            });
            ui.add_space(4.0);
            add = widgets::button(ui, "Добавить", false).clicked();
        });
        if pick {
            self.pick(ui, Picking::Game);
        }
        if add {
            let path = PathBuf::from(self.draft.fields.game_path.trim().trim_matches('"'));
            if self.add_game(&path) {
                self.draft.fields.game_path.clear();
            }
        }
    }

    /// Добавляет профиль игры в черновик. `false` — файл не подошёл, причина в строке состояния.
    fn add_game(&mut self, path: &Path) -> bool {
        let result = add_game(&mut self.draft.settings, path);
        let added = result.is_ok();
        self.set_status(result.map(|text| format!("{text} — нажмите «Применить»")));
        added
    }
}

/// Профиль для `path`. `Ok` — что сказать пользователю.
fn add_game(settings: &mut Settings, path: &Path) -> Result<String, String> {
    if path.as_os_str().is_empty() {
        return Err("укажите файл игры".to_string());
    }
    let is_exe = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    if !is_exe || !path.is_file() {
        return Err(format!("это не файл .exe: {}", path.display()));
    }
    let names: Vec<String> = settings.games.iter().map(|game| game.name.to_lowercase()).collect();
    let profile = crate::games::profile_for(path, |name| names.iter().any(|taken| taken == name));
    let text = if profile.arguments.is_empty() {
        format!("добавлено: {}", profile.name)
    } else {
        format!("добавлено: {}; параметры {} подставлены", profile.name, profile.arguments)
    };
    settings.games.push(profile);
    Ok(text)
}

fn general_page(ui: &mut Ui, settings: &mut Settings, info: &Context<'_>) {
    card(ui, "Основные параметры", |ui| {
        let overlay = &mut settings.overlay;
        switch_row(ui, "Показывать оверлей", None, &mut overlay.visible);
        switch_row(ui, "Поверх всех окон", None, &mut overlay.always_on_top);
        switch_row(
            ui,
            "Заблокировать оверлей",
            Some("Мышь проходит сквозь HUD и достаётся игре."),
            &mut overlay.locked,
        );
        switch_row(
            ui,
            "Заголовок с кнопками",
            Some("Название и кнопки «замок» и «скрыть» над HUD. У заблокированного не видны."),
            &mut overlay.show_header,
        );
        switch_row(ui, "Показывать время", None, &mut overlay.show_clock);
        switch_row(
            ui,
            "Режим «Только игра»",
            Some(
                "HUD виден, только пока в фокусе окно, от которого идут кадры, — то есть игра. \
                 Свернули игру или переключились на браузер — HUD прячется, как только кадры \
                 перестанут идти. Пока открыты настройки, он виден.",
            ),
            &mut overlay.game_only,
        );
        switch_row(
            ui,
            "Скрывать недоступные строки",
            Some(
                "Строка, которую эта машина ни разу не сообщила, исчезает вместо прочерка. \
                 Строка, хоть раз показавшая значение, место не теряет.",
            ),
            &mut settings.metrics.hide_unavailable,
        );
        let overlay = &mut settings.overlay;
        row(
            ui,
            "Непрозрачность фона",
            Some("Текст всегда непрозрачный — бледнеет только фон."),
            |ui| {
                widgets::slider_with_field(
                    ui,
                    &mut overlay.opacity,
                    MIN_OPACITY..=1.0,
                    0.01,
                    Unit::Percent,
                )
            },
        );
        row(
            ui,
            "Масштаб",
            Some("Размер HUD поверх масштаба Windows. Действует и на это окно."),
            |ui| {
                widgets::slider_with_field(
                    ui,
                    &mut overlay.scale,
                    MIN_SCALE..=MAX_SCALE,
                    SCALE_STEP,
                    Unit::Percent,
                )
            },
        );
        row(
            ui,
            "Режим оверлея",
            Some(
                "«Полноразмерный» — блоки столбцом со всеми строками и графиком. «Строка» — одна \
                 полоса: у блока главное число и пара значений, без заголовка с кнопками (замок \
                 и «скрыть» — в трее и на горячих клавишах).",
            ),
            |ui| {
                choice(ui, "overlay-mode", &mut overlay.mode, &OverlayMode::ALL, |mode| {
                    mode.title().to_string()
                });
            },
        );
        let strip = overlay.mode == OverlayMode::Strip;
        row(
            ui,
            "Высота полосы («Строка»)",
            Some("Высота полосы в точках, до масштаба."),
            |ui| {
                ui.add_enabled_ui(strip, |ui| {
                    widgets::slider_with_field(
                        ui,
                        &mut overlay.strip_height,
                        MIN_STRIP_HEIGHT..=MAX_STRIP_HEIGHT,
                        1.0,
                        Unit::Plain(""),
                    )
                });
            },
        );
        row(
            ui,
            "Ширина оверлея",
            Some("Ширина HUD относительно обычной. Длинные названия устройств помещаются лучше."),
            |ui| {
                // Полоса широка по содержимому — ширина действует только на столбец.
                ui.add_enabled_ui(!strip, |ui| {
                    widgets::slider_with_field(
                        ui,
                        &mut overlay.width_scale,
                        MIN_WIDTH_SCALE..=MAX_WIDTH_SCALE,
                        0.05,
                        Unit::Percent,
                    )
                });
            },
        );
        let sensors = &mut settings.sensors;
        row(
            ui,
            "Интервал обновления",
            Some(
                "Как часто обновляются загрузка и частоты. Температуры, мощность и память — не \
                 чаще раза в секунду. FPS считается всегда одинаково.",
            ),
            |ui| {
                choice(
                    ui,
                    "sensors-interval",
                    &mut sensors.hardware_interval_ms,
                    &mh_core::sensors::HARDWARE_INTERVALS_MS,
                    |ms| format!("{ms} мс"),
                );
            },
        );
        if !info.sensor_options_supported {
            warning(
                ui,
                "Служба старой версии интервал не принимает — обновите установку («Установка»).",
            );
        }
    });

    card(ui, "Единицы измерения", |ui| {
        let units = &mut settings.units;
        row(
            ui,
            "Температура",
            Some("Пороги предупреждений задаются в °C."),
            |ui| {
                choice(ui, "unit-temperature", &mut units.temperature, &Temperature::ALL, |unit| {
                    unit.title().to_string()
                });
            },
        );
        row(ui, "Частота", None, |ui| {
            choice(ui, "unit-frequency", &mut units.frequency, &Frequency::ALL, |unit| {
                unit.title().to_string()
            });
        });
        row(
            ui,
            "Память",
            Some("Память приложения меньше гигабайта всегда в МБ."),
            |ui| {
                choice(ui, "unit-memory", &mut units.memory, &Memory::ALL, |unit| {
                    unit.title().to_string()
                });
            },
        );
    });

    card(ui, "Расположение", |ui| {
        let anchor = &mut settings.overlay.anchor;
        switch_row(
            ui,
            "Закрепить оверлей на мониторе",
            Some(
                "HUD стоит в выбранном углу монитора и не перетаскивается. Отступы — в пикселях \
                 экрана от края всего монитора, панель задач не учитывается.",
            ),
            &mut anchor.enabled,
        );
        ui.add_enabled_ui(anchor.enabled, |ui| {
            let monitors: Vec<usize> = (0..info.monitors.len().max(1)).collect();
            row(ui, "Монитор", None, |ui| {
                choice(ui, "anchor-monitor", &mut anchor.monitor, &monitors, |index| {
                    info.monitors
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| format!("Монитор {index} (не подключён)"))
                });
            });
            row(ui, "Положение", None, |ui| {
                choice(ui, "anchor-corner", &mut anchor.corner, &Corner::ALL, |corner| {
                    corner.title().to_string()
                });
            });
            let centered = matches!(anchor.corner, Corner::TopCenter | Corner::BottomCenter);
            row(ui, "Отступ по горизонтали", None, |ui| {
                ui.add_enabled_ui(!centered, |ui| offset_slider(ui, &mut anchor.offset_x));
            });
            row(ui, "Отступ по вертикали", None, |ui| {
                offset_slider(ui, &mut anchor.offset_y)
            });
        });
    });

    card(ui, "Порядок блоков", |ui| {
        let blocks = &mut settings.blocks;
        let count = blocks.order.len();
        let mut shift = None;
        for (index, kind) in blocks.order.iter().enumerate() {
            let enabled = blocks.get(*kind).enabled;
            ui.push_id(index, |ui| {
                block_order_row(ui, *kind, enabled, index, count, &mut shift);
            });
        }
        if let Some((index, step)) = shift {
            blocks.shift(index, step);
        }
        hint(ui, "Выключенные блоки остаются в списке — включаются на своих страницах.");
    });

    card(ui, "Эксклюзивный полноэкранный режим", |ui| {
        let mode = &mut settings.overlay.fullscreen;
        row(
            ui,
            "Что делать с HUD",
            Some(
                "Поверх игры в этом режиме окна не видны, а перекрытая игра может перестать \
                 рисовать. Надёжнее всего — безрамочный режим в самой игре.",
            ),
            |ui| {
                egui::ComboBox::from_id_salt("fullscreen-mode")
                    .width(fit(ui, 260.0))
                    .selected_text(fullscreen_text(*mode))
                    .show_ui(ui, |ui| {
                        for option in [FullscreenMode::Hide, FullscreenMode::OtherMonitor] {
                            ui.selectable_value(mode, option, fullscreen_text(option));
                        }
                    });
            },
        );
    });
}

/// Ширина поля или списка: желаемая, но не шире того, что осталось в строке. Иначе при
/// крупном масштабе Windows карточка растягивается и уходит под предпросмотр.
fn fit(ui: &Ui, wanted: f32) -> f32 {
    // Запас на стрелку списка и рамку.
    (ui.available_width() - 28.0).clamp(80.0, wanted)
}

/// Выпадающий список из `options`. Значение, которого нет в списке, показывается как есть.
fn choice<T: Copy + PartialEq>(
    ui: &mut Ui,
    id: &str,
    value: &mut T,
    options: &[T],
    title: impl Fn(T) -> String,
) {
    egui::ComboBox::from_id_salt(id).width(fit(ui, 260.0)).selected_text(title(*value)).show_ui(
        ui,
        |ui| {
            for option in options {
                ui.selectable_value(value, *option, title(*option));
            }
        },
    );
}

fn offset_slider(ui: &mut Ui, offset: &mut i32) {
    let mut value = *offset as f32;
    let range = 0.0..=MAX_OFFSET as f32;
    if widgets::slider_with_field(ui, &mut value, range, 1.0, Unit::Plain(" px")).changed() {
        *offset = value.round() as i32;
    }
}

/// «⠿ Видеокарта ........ ↑ ↓».
fn block_order_row(
    ui: &mut Ui,
    kind: BlockKind,
    enabled: bool,
    index: usize,
    count: usize,
    shift: &mut Option<(usize, isize)>,
) {
    egui::Frame::new()
        .fill(theme::FIELD)
        .stroke(Stroke::new(1.0, theme::CARD_STROKE))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let color = if enabled { theme::TEXT_PRIMARY } else { theme::TEXT_DISABLED };
                ui.label(RichText::new(format!("{}.", index + 1)).color(theme::TEXT_DISABLED));
                ui.label(RichText::new(kind.title()).font(theme::bold(14.0)).color(color));
                if !enabled {
                    ui.label(
                        RichText::new("выключен")
                            .font(theme::regular(12.0))
                            .color(theme::TEXT_DISABLED),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if arrow_button(ui, false, index + 1 < count).on_hover_text("Ниже").clicked()
                    {
                        *shift = Some((index, 1));
                    }
                    ui.add_space(4.0);
                    if arrow_button(ui, true, index > 0).on_hover_text("Выше").clicked() {
                        *shift = Some((index, -1));
                    }
                });
            });
        });
    ui.add_space(4.0);
}

/// Кнопка со стрелкой. Стрелка рисуется сама: в Cuprum и запасных шрифтах egui её нет.
fn arrow_button(ui: &mut Ui, up: bool, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 22.0), sense);
    let hovered = enabled && response.hovered();
    let painter = ui.painter();
    let frame = if hovered { theme::ACCENT } else { theme::CARD_STROKE };
    painter.rect(
        rect,
        CornerRadius::same(3),
        theme::CARD,
        Stroke::new(1.0, frame),
        egui::StrokeKind::Inside,
    );
    let color = if enabled { theme::TEXT_PRIMARY } else { theme::TEXT_DISABLED };
    let center = rect.center();
    let (tip, base) = if up { (-4.0, 3.0) } else { (4.0, -3.0) };
    let points = vec![
        egui::pos2(center.x - 5.0, center.y + base),
        egui::pos2(center.x, center.y + tip),
        egui::pos2(center.x + 5.0, center.y + base),
    ];
    painter.add(egui::Shape::line(points, Stroke::new(1.8, color)));
    response
}

fn fullscreen_text(mode: FullscreenMode) -> &'static str {
    match mode {
        FullscreenMode::Hide => "Скрывать",
        FullscreenMode::OtherMonitor => "Переносить на другой монитор",
    }
}

fn fps_page(ui: &mut Ui, draft: &mut Draft, info: &Context<'_>) {
    card(ui, "Сейчас", |ui| {
        let state = &info.snapshot.fps;
        let target = state
            .target
            .as_ref()
            .map(|target| format!("{} (pid {})", target.executable, target.pid))
            .unwrap_or_else(|| "—".to_string());
        row(ui, "Цель", None, |ui| note(ui, &target));
        row(ui, "Состояние", None, |ui| note(ui, availability_text(state.availability)));
        if let Some(presentation) = &state.presentation {
            row(ui, "Вывод", None, |ui| note(ui, presentation));
        }
        if let Some(message) = state.message() {
            note(ui, message);
        }
        if let Some(detail) = &state.detail {
            // Сырые слова источника — здесь, в диагностике, а не в HUD.
            hint(ui, detail);
        }
        hint(ui, "Для захвата кадров нужна служба («Установка») или права администратора.");
    });

    let settings = &mut draft.settings;
    card(ui, "Показатели", |ui| {
        switch_row(ui, "Показывать блок FPS", None, &mut settings.blocks.fps.enabled);
        let enabled = settings.blocks.fps.enabled;
        for (metric, label) in [
            (Metric::FpsAverage, "AVG"),
            (Metric::FpsLow1, "1% Low"),
            (Metric::FpsLow01, "0.1% Low"),
        ] {
            metric_switch(ui, settings, metric, label, enabled);
        }
        metric_switch_help(
            ui,
            settings,
            Metric::FpsLatency,
            "Задержка (Latency)",
            Some(
                "Среднее за секунду время от начала работы CPU над кадром до его появления на \
                 экране — как «Display Latency» у PresentMon. Без PresentMon (запасной захват) \
                 недоступна.",
            ),
            enabled,
        );
        metric_switch_help(
            ui,
            settings,
            Metric::FpsApi,
            "API и разрешение",
            Some("Графический API по данным PresentMon и размер окна игры в пикселях."),
            enabled,
        );
        metric_switch_help(
            ui,
            settings,
            Metric::FpsTarget,
            "Процесс",
            Some("Имя измеряемого процесса под цифрами. Причина, почему цифр нет, видна всегда."),
            enabled,
        );
        switch_row_enabled(
            ui,
            "График времени кадра",
            None,
            &mut settings.overlay.show_graph,
            enabled,
        );
    });

    card(ui, "Цель", |ui| {
        let fps = &mut settings.fps;
        row(
            ui,
            "Что измерять",
            Some("Окно в фокусе меняется само, выбранный процесс — только по имени или PID."),
            |ui| {
                egui::ComboBox::from_id_salt("fps-target")
                    .width(fit(ui, 260.0))
                    .selected_text(target_text(fps.target))
                    .show_ui(ui, |ui| {
                        for option in [TargetChoice::Auto, TargetChoice::Manual] {
                            ui.selectable_value(&mut fps.target, option, target_text(option));
                        }
                    });
            },
        );
        if fps.target == TargetChoice::Manual {
            let fields = &mut draft.fields;
            text_row(ui, "Имя процесса", None, &mut fields.manual_process, "game.exe");
            text_row(
                ui,
                "PID",
                Some("Необязательно: нужен, если запущено несколько копий."),
                &mut fields.manual_pid,
                "",
            );
        }
    });

    card(ui, "PresentMon", |ui| {
        text_row(
            ui,
            "Свой PresentMon.exe",
            Some(
                "Действует только без службы: служба запускает лишь вложенный — от имени \
                 системы чужой файл не запускается. Вступает в силу после перезапуска.",
            ),
            &mut draft.fields.presentmon_path,
            "пусто — вложенный 2.5.1",
        );
    });

    appearance_card(ui, &mut draft.settings.blocks.fps, "FPS", None, true);
}

fn target_text(choice: TargetChoice) -> &'static str {
    match choice {
        TargetChoice::Auto => "Окно в фокусе",
        TargetChoice::Manual => "Выбранный процесс",
    }
}

fn gpu_page(ui: &mut Ui, settings: &mut Settings, info: &Context<'_>, gpus: &[String]) {
    card(ui, "Видеокарта для мониторинга", |ui| {
        let chosen = &mut settings.sensors.gpu;
        let mut options: Vec<Option<String>> = vec![None];
        options.extend(gpus.iter().cloned().map(Some));
        if let Some(missing) = chosen.clone().filter(|name| !gpus.contains(name)) {
            // Выбранной карты сейчас нет — она остаётся в списке, чтобы выбор не пропал молча.
            options.push(Some(missing));
        }
        row(
            ui,
            "Видеокарта",
            Some(
                "«Авто» — карта с наибольшей видеопамятью. Выбранной карты нет в системе — \
                 читается автоматическая.",
            ),
            |ui| {
                egui::ComboBox::from_id_salt("sensor-gpu")
                    .width(fit(ui, 320.0))
                    .selected_text(gpu_title(chosen.as_deref(), gpus))
                    .show_ui(ui, |ui| {
                        for option in options {
                            let title = gpu_title(option.as_deref(), gpus);
                            ui.selectable_value(chosen, option, title);
                        }
                    });
            },
        );
        if !info.sensor_options_supported {
            warning(ui, "Служба старой версии выбор не принимает — обновите установку.");
        }
    });
    card(ui, "Показатели", |ui| {
        switch_row(ui, "Показывать блок «Видеокарта»", None, &mut settings.blocks.gpu.enabled);
        let enabled = settings.blocks.gpu.enabled;
        hint(ui, "Загрузка видна в заголовке блока всегда.");
        for (metric, label) in [
            (Metric::GpuTemperature, "Температура"),
            (Metric::GpuClock, "Частота ядра"),
            (Metric::GpuMemoryClock, "Частота памяти"),
            (Metric::GpuVram, "Видеопамять"),
            (Metric::GpuPower, "Потребление"),
            (Metric::GpuFan, "Обороты вентилятора"),
        ] {
            metric_switch(ui, settings, metric, label, enabled);
        }
        metric_switch_help(
            ui,
            settings,
            Metric::GpuFanPercent,
            "Обороты вентилятора в %",
            Some("Доля от максимума. Сообщают только карты NVIDIA."),
            enabled,
        );
        hint(
            ui,
            "Напряжение и температуры hotspot и памяти карты NVIDIA через открытый интерфейс \
             драйвера не сообщают.",
        );
    });
    let device = info.snapshot.gpu.name.as_deref().map(format::short_device_name);
    appearance_card(ui, &mut settings.blocks.gpu, "GPU", Some(device), false);
    thresholds_card(ui, &mut settings.blocks.gpu, "GPU");
}

fn cpu_page(ui: &mut Ui, settings: &mut Settings, info: &Context<'_>) {
    card(ui, "Показатели", |ui| {
        switch_row(ui, "Показывать блок «Процессор»", None, &mut settings.blocks.cpu.enabled);
        let enabled = settings.blocks.cpu.enabled;
        hint(ui, "Загрузка видна в заголовке блока всегда.");
        for (metric, label) in [
            (Metric::CpuTemperature, "Температура"),
            (Metric::CpuClock, "Частота"),
            (Metric::CpuPower, "Потребление"),
        ] {
            metric_switch(ui, settings, metric, label, enabled);
        }
        let hybrid = info.snapshot.cpu.hybrid_clocks().is_some();
        let hybrid_help = if hybrid {
            "Средняя частота производительных и энергоэффективных ядер."
        } else {
            "Только у гибридных процессоров Intel; у этого процессора ядра одного типа, строк не \
             будет. Малые ядра Core Ultra (LP-E) Windows относит к E-ядрам."
        };
        metric_switch_help(
            ui,
            settings,
            Metric::CpuHybridClock,
            "Частота P- и E-ядер",
            Some(hybrid_help),
            enabled,
        );
        metric_switch_help(
            ui,
            settings,
            Metric::CpuCoreLoad,
            "График загрузки по ядрам",
            Some("Столбик на каждое логическое ядро; E-ядра бледнее."),
            enabled,
        );
        metric_switch_help(
            ui,
            settings,
            Metric::CpuCoreClock,
            "Частоты по ядрам",
            Some("Сетка по четыре ядра в ряд; единица — из «Общих»."),
            enabled,
        );
        hint(
            ui,
            "Температура и потребление читаются через PawnIO — нужна служба или права администратора.",
        );
    });
    let device = info.snapshot.cpu.name.as_deref().map(format::short_device_name);
    appearance_card(ui, &mut settings.blocks.cpu, "CPU", Some(device), false);
    thresholds_card(ui, &mut settings.blocks.cpu, "CPU");
}

fn memory_page(ui: &mut Ui, settings: &mut Settings) {
    card(ui, "Показатели", |ui| {
        switch_row(ui, "Показывать блок «ОЗУ»", None, &mut settings.blocks.ram.enabled);
        let enabled = settings.blocks.ram.enabled;
        hint(ui, "Процент занятой памяти виден в заголовке блока всегда.");
        metric_switch(ui, settings, Metric::RamUsed, "Занято / всего", enabled);
        metric_switch_help(
            ui,
            settings,
            Metric::RamProcess,
            "Память захваченного приложения",
            Some("Частный рабочий набор процесса — как в колонке «Память» диспетчера задач."),
            enabled,
        );
        metric_switch_help(
            ui,
            settings,
            Metric::RamSpeed,
            "Скорость памяти",
            Some("Рабочая частота из SMBIOS — с XMP/EXPO разогнанная, а не паспортная."),
            enabled,
        );
    });
    appearance_card(ui, &mut settings.blocks.ram, "RAM", None, false);
}

/// Имена видеокарт, как их называет ядро графики. Программный рендер — не видеокарта.
fn gpu_names() -> Vec<String> {
    let mut names: Vec<String> = mh_platform::gpu::adapters()
        .into_iter()
        .filter(|adapter| !adapter.software && !adapter.name.is_empty())
        .map(|adapter| adapter.name)
        .collect();
    names.dedup();
    names
}

fn gpu_title(choice: Option<&str>, present: &[String]) -> String {
    match choice {
        None => "Авто (рекомендуется)".to_string(),
        Some(name) if present.iter().any(|gpu| gpu == name) => name.to_string(),
        Some(name) => format!("{name} (не найдена)"),
    }
}

/// Название и цвета блока.
///
/// `device` — у блоков с устройством: `Some(имя, если система его сообщила)`. `accent` — у FPS:
/// крупная цифра и график.
fn appearance_card(
    ui: &mut Ui,
    block: &mut Block,
    standard: &str,
    device: Option<Option<String>>,
    accent: bool,
) {
    card(ui, "Внешний вид", |ui| {
        if let Some(name) = &device {
            let help = match name {
                Some(name) => format!("Сейчас система называет устройство «{name}»."),
                None => "Система пока не сообщила имя устройства.".to_string(),
            };
            switch_row(
                ui,
                &format!("Название устройства вместо «{standard}»"),
                Some(&help),
                &mut block.device_name,
            );
        }
        let fallback = Block { title: String::new(), ..block.clone() };
        let placeholder = fallback.heading(standard, device.flatten().as_deref());
        row(
            ui,
            "Своё название",
            Some("Пусто — название выше."),
            |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut block.title)
                        .hint_text(RichText::new(placeholder).color(theme::TEXT_DISABLED))
                        .char_limit(MAX_TITLE_CHARS)
                        .desired_width(fit(ui, 280.0)),
                );
            },
        );
        let colors = &mut block.colors;
        widgets::color_row(ui, "Цвет заголовка", &mut colors.header, theme::DEFAULT_HEADER);
        widgets::color_row(ui, "Цвет подписей", &mut colors.labels, theme::DEFAULT_LABELS);
        widgets::color_row(ui, "Цвет значений", &mut colors.values, theme::DEFAULT_VALUES);
        if accent {
            widgets::color_row(ui, "Цвет FPS и графика", &mut colors.accent, theme::DEFAULT_ACCENT);
        }
        if !colors.is_default() {
            ui.add_space(4.0);
            if widgets::button(ui, "Цвета темы", false).clicked() {
                *colors = Default::default();
            }
        }
    });
}

/// Когда температура и загрузка меняют цвет.
fn thresholds_card(ui: &mut Ui, block: &mut Block, what: &str) {
    card(ui, "Пороги предупреждений", |ui| {
        let thresholds = &mut block.thresholds;
        let load_help = format!(
            "Загрузка в заголовке желтеет от {LOAD_WARN_PERCENT:.0}% и краснеет от              {LOAD_CRITICAL_PERCENT:.0}%."
        );
        switch_row(
            ui,
            &format!("Цветовая индикация загрузки {what}"),
            Some(&load_help),
            &mut thresholds.color_load,
        );
        let range = MIN_TEMPERATURE..=MAX_TEMPERATURE;
        let warn_changed = row(
            ui,
            "Температура предупреждения",
            Some("С этой температуры значение желтеет."),
            |ui| {
                widgets::slider_with_field(
                    ui,
                    &mut thresholds.warn_c,
                    range.clone(),
                    1.0,
                    Unit::Plain(" °C"),
                )
            },
        )
        .changed();
        let critical_changed = row(
            ui,
            "Температура критическая",
            Some("С этой — краснеет. Не ниже температуры предупреждения."),
            |ui| {
                widgets::slider_with_field(
                    ui,
                    &mut thresholds.critical_c,
                    range.clone(),
                    1.0,
                    Unit::Plain(" °C"),
                )
            },
        )
        .changed();
        // Порядок держится сам: двигаемый порог тянет за собой второй.
        if thresholds.critical_c < thresholds.warn_c {
            if warn_changed {
                thresholds.critical_c = thresholds.warn_c;
            } else if critical_changed {
                thresholds.warn_c = thresholds.critical_c;
            }
        }
    });
}

fn hotkeys_page(ui: &mut Ui, fields: &mut Drafts, info: &Context<'_>) {
    card(ui, "Глобальные сочетания", |ui| {
        text_row(ui, "Показать / скрыть", None, &mut fields.toggle_visibility, "Ctrl+Shift+F11");
        text_row(ui, "Заблокировать", None, &mut fields.toggle_lock, "Ctrl+Shift+F10");
        hint(ui, "Новые сочетания начинают работать после перезапуска MH Monitoring.");
        for error in info.hotkey_errors {
            warning(ui, error);
        }
    });
}

fn about_page(ui: &mut Ui, info: &Context<'_>) {
    card(ui, "MH Monitoring", |ui| {
        row(ui, "Версия", None, |ui| note(ui, env!("CARGO_PKG_VERSION")));
        row(ui, "Файл настроек", None, |ui| {
            note(ui, &info.settings_path.display().to_string())
        });
        hint(ui, "PresentMon 2.5.1 (MIT) и модуль PawnIO AMDFamily17 (LGPL-2.1) встроены.");
        hint(ui, "Шрифт Cuprum — SIL Open Font License 1.1.");
    });
    let snapshot = info.snapshot;
    card(ui, "Датчики", |ui| {
        row(ui, "Железо", None, |ui| note(ui, status_text(snapshot.hardware_status)));
        for (name, health) in [
            ("Видеокарта", snapshot.gpu.health),
            ("Процессор", snapshot.cpu.health),
            ("ОЗУ", snapshot.memory.health),
        ] {
            let reason = health.reason.map(|r| format!(" — {r}")).unwrap_or_default();
            row(ui, name, None, |ui| note(ui, &format!("{}{reason}", status_text(health.status))));
        }
    });
}

fn metric_switch(ui: &mut Ui, settings: &mut Settings, metric: Metric, label: &str, enabled: bool) {
    metric_switch_help(ui, settings, metric, label, None, enabled);
}

fn metric_switch_help(
    ui: &mut Ui,
    settings: &mut Settings,
    metric: Metric,
    label: &str,
    help: Option<&str>,
    enabled: bool,
) {
    let mut on = settings.metrics.is_enabled(metric);
    if switch_row_enabled(ui, label, help, &mut on, enabled).changed() {
        settings.metrics.set_enabled(metric, on);
    }
}

fn text_row(ui: &mut Ui, label: &str, help: Option<&str>, value: &mut String, placeholder: &str) {
    row(ui, label, help, |ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(RichText::new(placeholder).color(theme::TEXT_DISABLED))
                .desired_width(fit(ui, 280.0)),
        );
    });
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

/// Цвета полей и выделения окна. Стиль меняется только у этого окна: HUD рисует своими.
fn style_window(ui: &mut Ui) {
    let visuals = &mut ui.style_mut().visuals;
    visuals.extreme_bg_color = theme::FIELD;
    visuals.selection.bg_fill = theme::ACCENT.gamma_multiply(0.45);
    visuals.selection.stroke = Stroke::new(1.0, theme::ACCENT);
    visuals.slider_trailing_fill = true;
    for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered] {
        widget.weak_bg_fill = theme::FIELD;
        widget.bg_stroke = Stroke::new(1.0, theme::CARD_STROKE);
    }
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.6));
}

fn panel_frame(fill: Color32, margin: Margin) -> egui::Frame {
    egui::Frame::new().fill(fill).inner_margin(margin)
}

/// Пункт списка разделов на всю ширину панели.
fn nav_item(ui: &mut Ui, title: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(4), theme::CARD);
        let bar = egui::Rect::from_min_size(rect.min, vec2(3.0, rect.height()));
        painter.rect_filled(bar, CornerRadius::same(1), theme::ACCENT);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(4), theme::CARD.gamma_multiply(0.6));
    }
    let color = if selected { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    painter.text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        theme::regular(15.0),
        color,
    );
    ui.add_space(2.0);
    response
}

fn side_button(ui: &mut Ui, text: &str) -> egui::Response {
    let response = ui.add_sized(
        [ui.available_width(), 28.0],
        egui::Button::new(RichText::new(text).font(theme::regular(13.0)))
            .fill(theme::FIELD)
            .stroke(Stroke::new(1.0, theme::CARD_STROKE)),
    );
    ui.add_space(4.0);
    response
}

pub(crate) fn group(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).font(theme::bold(16.0)).color(theme::TEXT_PRIMARY));
    ui.add_space(4.0);
}

pub(crate) fn note(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::TEXT_SECONDARY));
}

pub(crate) fn hint(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text).font(theme::regular(12.0)).color(theme::TEXT_DISABLED),
        )
        .wrap(),
    );
}

pub(crate) fn warning(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(theme::ACCENT_WARN)).wrap());
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

    /// Пока черновик не трогали, он идёт за рабочими настройками целиком.
    #[test]
    fn an_untouched_draft_follows_the_live_settings() {
        let mut live = Settings::default();
        let mut draft = Draft::of(&live);
        live.overlay.locked = true;
        live.overlay.x = Some(40);
        draft.follow(&live);
        assert_eq!(draft.settings, live);
        assert!(!draft.is_dirty());
    }

    /// Правки пользователя переживают перемены мимо окна, а позиция HUD — всегда рабочая.
    #[test]
    fn user_edits_survive_outside_changes() {
        let mut live = Settings::default();
        let mut draft = Draft::of(&live);
        draft.settings.overlay.opacity = 0.5;
        draft.settings.overlay.locked = true;

        live.overlay.x = Some(-1_800);
        live.overlay.locked = false;
        live.overlay.visible = false;
        draft.follow(&live);

        assert_eq!(draft.settings.overlay.opacity, 0.5);
        assert!(draft.settings.overlay.locked, "замок выбран в окне — остаётся");
        assert!(!draft.settings.overlay.visible, "видимость в окне не трогали — идёт за треем");
        assert_eq!(draft.settings.overlay.x, Some(-1_800));
        assert!(draft.is_dirty());
    }

    #[test]
    fn text_fields_make_the_draft_dirty_and_are_checked_on_apply() {
        let live = Settings::default();
        let mut draft = Draft::of(&live);
        draft.fields.game_path = r"C:\game.exe".into();
        assert!(!draft.is_dirty(), "добавляемая игра — не настройка");

        draft.fields.manual_pid = "abc".into();
        assert!(draft.is_dirty());
        assert!(draft.result().unwrap_err().contains("abc"));

        draft.fields.manual_pid = "77".into();
        assert_eq!(draft.result().unwrap().fps.manual_pid, Some(77));
    }

    /// «Сбросить» и «Загрузить» не трогают место HUD и пройденный первый запуск.
    #[test]
    fn replacing_keeps_what_belongs_to_this_machine() {
        let mut live = Settings::default();
        live.overlay.x = Some(10);
        live.overlay.y = Some(20);
        live.setup_done = true;
        live.overlay.opacity = 0.4;
        let mut draft = Draft::of(&live);

        let mut incoming = Settings::default();
        incoming.overlay.x = Some(999);
        incoming.hotkeys.toggle_lock = "Alt+F8".into();
        draft.replace(incoming);

        let result = draft.result().unwrap();
        assert_eq!((result.overlay.x, result.overlay.y), (Some(10), Some(20)));
        assert!(result.setup_done);
        assert_eq!(result.overlay.opacity, Settings::default().overlay.opacity);
        assert_eq!(result.hotkeys.toggle_lock, "Alt+F8", "поля ввода взяты из нового");
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
    fn only_existing_executables_become_games() {
        let mut settings = Settings::default();
        assert!(add_game(&mut settings, Path::new("")).is_err());
        assert!(add_game(&mut settings, Path::new(r"C:\no\such\game.exe")).is_err());
        assert!(add_game(&mut settings, &std::env::temp_dir()).is_err());

        let this_exe = std::env::current_exe().unwrap();
        add_game(&mut settings, &this_exe).unwrap();
        add_game(&mut settings, &this_exe).unwrap();
        assert_eq!(settings.games.len(), 2);
        assert_ne!(settings.games[0].name, settings.games[1].name, "имена не повторяются");
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

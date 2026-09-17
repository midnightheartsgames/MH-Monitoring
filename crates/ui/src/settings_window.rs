//! Окно настроек (PLAN.md §6/P5).
//!
//! Флажки, ползунки и переключатели применяются сразу — их видно на HUD. Текстовые поля живут
//! черновиком и применяются кнопкой или Enter: иначе каждая набранная буква меняла бы цель и
//! перезапускала захват.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{
    self, Color32, Pos2, RichText, ScrollArea, Slider, Ui, ViewportBuilder, ViewportClass,
    ViewportCommand, ViewportId, WindowLevel,
};
use mh_core::{FpsAvailability, SensorStatus, Snapshot};

use crate::controls::parse_hotkey;
use crate::settings::{FullscreenMode, Metric, SCALES, Settings, TargetChoice};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Overlay,
    Rows,
    Fps,
    Games,
    Hotkeys,
    Service,
    About,
}

impl Page {
    const ALL: [Page; 7] = [
        Page::Overlay,
        Page::Rows,
        Page::Fps,
        Page::Games,
        Page::Hotkeys,
        Page::Service,
        Page::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Overlay => "Оверлей",
            Page::Rows => "Строки",
            Page::Fps => "FPS",
            Page::Games => "Игры",
            Page::Hotkeys => "Хоткеи",
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
    /// Состояние установки — с отметкой, когда проверено: спрашивать систему на каждом кадре
    /// незачем.
    install_state: Option<InstallState>,
    /// Где открыть окно — рядом с HUD, а не под ним: HUD висит поверх всех окон.
    position: Option<Pos2>,
    /// Вывести окно вперёд на ближайшем кадре: открыли из трея или хоткеем поверх игры.
    focus_pending: bool,
    /// Открыт диалог выбора файла игры; ответ придёт сюда.
    picking: Option<Receiver<Option<PathBuf>>>,
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
        self.focus_pending = true;
    }

    /// Рисует окно, если оно открыто. Вызывать из `ui` корневого окна каждый кадр.
    /// Возвращает просьбу об операции со службой, если кнопку нажали.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        settings: &mut Settings,
        info: &Context<'_>,
    ) -> Option<ServiceRequest> {
        if !self.open {
            return None;
        }
        let mut builder = ViewportBuilder::default()
            .with_title("MH Monitoring — настройки")
            .with_inner_size(SIZE)
            .with_min_inner_size([480.0, 360.0])
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
            let mut request = None;
            egui::CentralPanel::default().show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| match self.page {
                    Page::Overlay => overlay_page(ui, settings),
                    Page::Rows => rows_page(ui, settings),
                    Page::Fps => self.fps_page(ui, settings, info),
                    Page::Games => self.games_page(ui, settings),
                    Page::Hotkeys => self.hotkeys_page(ui, settings, info),
                    Page::Service => request = self.service_page(ui, info),
                    Page::About => about_page(ui, info),
                });
            });
            request
        })
    }

    /// Открывает окно сразу на нужном разделе — например, на «Установке» при первом запуске.
    pub fn open_page(&mut self, settings: &Settings, position: Option<Pos2>, page: Page) {
        self.open(settings, position);
        self.page = page;
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

        group(ui, "MH Monitoring");
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
                 MH-Monitoring-Service.exe): захват кадров и температура CPU работают без прав \
                 администратора. Подтверждение UAC спросят один раз.",
                crate::installer::install_dir().display()
            ),
        );
        ui.add_space(6.0);
        let mut request = None;
        ui.add_enabled_ui(!info.service_busy, |ui| {
            ui.horizontal(|ui| {
                let label = match (&state.installed_version, state.running_installed) {
                    (None, _) => Some("Установить"),
                    (Some(_), false) => Some("Обновить установленную"),
                    (Some(_), true) => None,
                };
                if let Some(label) = label
                    && ui.button(label).clicked()
                {
                    request = Some(ServiceRequest::Install);
                }
                if state.installed_version.is_some() && ui.button("Удалить").clicked() {
                    request = Some(ServiceRequest::Uninstall);
                }
            });
        });
        if request.is_some() {
            self.install_state = None;
        }
        if let Some(status) = info.service_status {
            ui.add_space(4.0);
            note(ui, &format!("Последняя операция: {status}"));
        }

        group(ui, "Запуск");
        let mut autostart = state.autostart;
        if ui.checkbox(&mut autostart, "Запускать вместе с Windows").changed() {
            match crate::installer::set_autostart(autostart) {
                Ok(()) => {
                    if let Some(state) = self.install_state.as_mut() {
                        state.autostart = autostart;
                    }
                }
                Err(error) => {
                    self.status = Some((Page::Service, format!("автозапуск: {error}"), true))
                }
            }
        }
        self.status_line(ui, Page::Service);

        group(ui, "Служба");
        note(ui, &format!("Снимки сейчас: {}", info.backend));
        match &state.service_exe {
            Some(path) => {
                hint(ui, &format!("Служба запускает: {}", path.display()));
                if !state.service_running {
                    warning(ui, "Служба остановлена: без неё кадры и температура CPU недоступны.");
                    if ui
                        .add_enabled(!info.service_busy, egui::Button::new("Запустить службу"))
                        .clicked()
                    {
                        request = Some(ServiceRequest::StartService);
                        self.install_state = None;
                    }
                }
                if !crate::installer::same_path(path, &crate::installer::installed_service_exe()) {
                    warning(
                        ui,
                        "Служба стоит не на установленной копии. Если этот файл лежит там, куда \
                         пишут обычные пользователи, его подмена даст права системы — \
                         переустановите программу.",
                    );
                }
            }
            None => hint(ui, "Служба не установлена."),
        }
        request
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
        hint(ui, "Для захвата кадров нужна служба («Установка») или права администратора.");

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
        hint(
            ui,
            "Свой PresentMon действует только без службы: служба запускает лишь вложенный —              от имени системы чужой файл не запускается.",
        );
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

    fn games_page(&mut self, ui: &mut Ui, settings: &mut Settings) {
        self.poll_picker(settings);

        group(ui, "Старые игры без безрамочного режима");
        hint(
            ui,
            "MH Monitoring запускает игру с параметрами (у Warcraft III 1.26 — -window) и, если \
             отмечено, снимает с окна игры рамку и растягивает его на весь монитор. HUD тогда \
             виден поверх игры. В игру ничего не внедряется: меняется только её окно.",
        );
        hint(
            ui,
            "Ярлык на рабочем столе запускает игру вместе с оверлеем. Картинка игры 4:3 на \
             широком мониторе растягивается.",
        );

        enum Action {
            Launch(usize),
            Shortcut(usize),
            Remove(usize),
        }
        let mut action = None;
        if settings.games.is_empty() {
            note(ui, "Игр пока нет.");
        }
        for (index, game) in settings.games.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(&game.name).font(theme::bold(15.0)).color(theme::TEXT_PRIMARY),
                );
                hint(ui, &game.path);
                ui.horizontal(|ui| {
                    ui.label("Параметры:");
                    ui.add(egui::TextEdit::singleline(&mut game.arguments).desired_width(260.0));
                });
                ui.checkbox(&mut game.borderless, "Без рамки на весь монитор");
                if let Some(window) = &game.window_exe {
                    hint(ui, &format!("Рамка снимается с окна {window}."));
                }
                ui.horizontal(|ui| {
                    if ui.button("Запустить").clicked() {
                        action = Some(Action::Launch(index));
                    }
                    if ui.button("Ярлык на рабочем столе").clicked() {
                        action = Some(Action::Shortcut(index));
                    }
                    if ui.button("Удалить").clicked() {
                        action = Some(Action::Remove(index));
                    }
                });
            });
        }
        match action {
            Some(Action::Launch(index)) => {
                let result = crate::games::launch(&settings.games[index]);
                self.report(Page::Games, result, "игра запущена");
            }
            Some(Action::Shortcut(index)) => {
                let result = crate::games::create_desktop_shortcut(&settings.games[index]);
                let text = match &result {
                    Ok(link) => format!("ярлык создан: {}", link.display()),
                    Err(_) => String::new(),
                };
                self.report(Page::Games, result.map(|_| ()), &text);
            }
            Some(Action::Remove(index)) => {
                let removed = settings.games.remove(index);
                self.report(Page::Games, Ok(()), &format!("удалено: {}", removed.name));
            }
            None => {}
        }

        group(ui, "Добавить игру");
        ui.add_enabled_ui(self.picking.is_none(), |ui| {
            if ui.button("Выбрать файл игры…").clicked() {
                let (sender, receiver) = mpsc::channel();
                let repaint = ui.ctx().clone();
                let spawned =
                    std::thread::Builder::new().name("mh-pick-game".into()).spawn(move || {
                        let picked = mh_platform::dialog::pick_executable("Файл игры");
                        let _ = sender.send(picked);
                        repaint.request_repaint_of(ViewportId::ROOT);
                    });
                if spawned.is_ok() {
                    self.picking = Some(receiver);
                }
            }
        });
        text_field(ui, "Или путь к .exe", &mut self.drafts.game_path, r"C:\Games\game.exe");
        ui.add_space(4.0);
        if ui.button("Добавить").clicked() {
            let path = PathBuf::from(self.drafts.game_path.trim().trim_matches('"'));
            if self.add_game(settings, &path) {
                self.drafts.game_path.clear();
            }
        }
        self.status_line(ui, Page::Games);
    }

    fn poll_picker(&mut self, settings: &mut Settings) {
        let Some(receiver) = &self.picking else { return };
        match receiver.try_recv() {
            Ok(Some(path)) => {
                self.picking = None;
                self.add_game(settings, &path);
            }
            Ok(None) | Err(TryRecvError::Disconnected) => self.picking = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Добавляет профиль игры. `false` — файл не подошёл, причина в строке состояния.
    fn add_game(&mut self, settings: &mut Settings, path: &Path) -> bool {
        let result = add_game(settings, path);
        let added = result.is_ok();
        let text = result.as_ref().cloned().unwrap_or_default();
        self.report(Page::Games, result.map(|_| ()), &text);
        added
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
            warning(ui, "Часть изменений вступит в силу после перезапуска MH Monitoring.");
        }
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

    group(ui, "Эксклюзивный полноэкранный режим");
    ui.radio_value(&mut overlay.fullscreen, FullscreenMode::Hide, "Скрывать HUD");
    ui.radio_value(
        &mut overlay.fullscreen,
        FullscreenMode::OtherMonitor,
        "Переносить HUD на другой монитор",
    );
    hint(
        ui,
        "Поверх игры в этом режиме окна не видны, а перекрытая игра может перестать рисовать. \
         На время режима HUD уходит с её монитора; без второго монитора — прячется. Надёжнее \
         всего — безрамочный режим в самой игре.",
    );
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
    group(ui, "MH Monitoring");
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

pub(crate) fn group(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).font(theme::bold(16.0)).color(theme::TEXT_PRIMARY));
    ui.add_space(4.0);
}

pub(crate) fn note(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::TEXT_SECONDARY));
}

pub(crate) fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::regular(12.0)).color(theme::TEXT_DISABLED));
}

pub(crate) fn warning(ui: &mut Ui, text: &str) {
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

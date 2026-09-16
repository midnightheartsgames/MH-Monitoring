//! Окно оверлея: связывает движок, HUD, окно настроек, трей и хоткеи.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Id, Pos2, Rect, Sense, Vec2, ViewportBuilder, ViewportCommand, WindowLevel, pos2,
};
use mh_engine::{Engine, EngineConfig, extract_presentmon};
use mh_platform::overlay::{OverlayWindow, ScreenRect, is_on_any_monitor, work_area_near};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::controls::{Command, Controls};
use crate::diag;
use crate::hud::{self, HudActions, SeenRows};
use crate::settings::{self, Settings};
use crate::settings_window::{self, SettingsWindow};
use crate::theme;

/// Как часто оверлей снова забирает верх z-порядка и проверяет своё место на экране.
const HOUSEKEEPING: Duration = Duration::from_secs(1);
/// Настройки пишутся не на каждое движение ползунка, а когда всё успокоилось.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// Куда ставить HUD, если сохранённой позиции нет или она на отключённом мониторе.
const DEFAULT_POSITION: (i32, i32) = (20, 20);
/// Оценка размера окна для проверки позиции до первого кадра, в физических пикселях.
const ESTIMATED_SIZE: (i32, i32) = (272, 420);

pub fn run() -> eframe::Result {
    diag::start(settings::local_dir().join("mh-monitor.log"));
    diag::log(format!("MH Monitor {}", env!("CARGO_PKG_VERSION")));
    let settings_path = settings::default_path();
    let loaded =
        diag::timed("настройки прочитаны", || Settings::load(&settings_path));
    if let Some(notice) = &loaded.notice {
        diag::log(format!("настройки: {notice}"));
    }
    let settings = loaded.settings;
    // Не разложился — PresentMon будет «не найден», и движок честно откатится на свой ETW.
    let bin = settings::local_dir().join("bin");
    let presentmon = diag::timed("PresentMon разложен", || extract_presentmon(&bin))
        .unwrap_or_else(|error| {
            diag::log(format!("PresentMon не разложен: {error}"));
            bin.join("missing-presentmon.exe")
        });

    let overlay = &settings.overlay;
    let level = if overlay.always_on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal };
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("MH Monitor")
            .with_decorations(false)
            .with_transparent(true)
            .with_window_level(level)
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false)
            .with_inner_size([theme::OVERLAY_WIDTH, ESTIMATED_SIZE.1 as f32])
            .with_mouse_passthrough(overlay.locked)
            .with_visible(overlay.visible),
        persist_window: false,
        ..Default::default()
    };

    let notice = loaded.notice;
    eframe::run_native(
        "MH Monitor",
        options,
        Box::new(move |cc| {
            Ok(Box::new(OverlayApp::new(cc, settings, settings_path, presentmon, notice)))
        }),
    )
}

/// Что из настроек уже доведено до окна и движка.
#[derive(Debug, Clone, PartialEq)]
struct Applied {
    visible: bool,
    locked: bool,
    always_on_top: bool,
    scale: f32,
    target: mh_core::TargetSettings,
}

impl Applied {
    fn of(settings: &Settings) -> Applied {
        Applied {
            visible: settings.overlay.visible,
            locked: settings.overlay.locked,
            always_on_top: settings.overlay.always_on_top,
            scale: settings.overlay.scale,
            target: settings.fps.target_settings(),
        }
    }
}

struct OverlayApp {
    /// `None` после выхода: движок останавливается явно, в `on_exit`.
    engine: Option<Engine>,
    settings: Settings,
    /// Настройки на момент последней записи — чтобы писать только изменения.
    saved: Settings,
    settings_path: PathBuf,
    save_due: Option<Instant>,
    settings_window: SettingsWindow,
    controls: Controls,
    window: Option<OverlayWindow>,
    applied: Option<Applied>,
    next_housekeeping: Instant,
    /// Где HUD был в прошлом кадре — по этой области его таскают.
    hud_rect: Option<Rect>,
    /// Последний запрошенный размер окна и масштаб, при котором он запрошен.
    last_size: Option<(Vec2, f32)>,
    seen_rows: SeenRows,
    /// Сообщения приложения внизу HUD: хоткеи, сброшенные настройки.
    notes: Vec<String>,
    /// Последнее записанное в журнал состояние кадров — пишем только перемены.
    logged_fps: String,
    /// Масштаб экрана под HUD — для перевода физических пикселей в точки egui.
    pixels_per_point: f32,
    exiting: bool,
}

impl OverlayApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        mut settings: Settings,
        settings_path: PathBuf,
        presentmon: PathBuf,
        notice: Option<String>,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        ctx.set_fonts(theme::fonts());
        // Масштаб — до первого кадра: иначе первый запрос размера окна уйдёт по масштабу 100 %.
        ctx.set_zoom_factor(settings.overlay.scale);

        let window = match cc.window_handle().map(|handle| handle.as_raw()) {
            // SAFETY: HWND только что создан eframe для этого окна и живёт вместе с приложением.
            Ok(RawWindowHandle::Win32(handle)) => {
                Some(unsafe { OverlayWindow::from_raw(handle.hwnd.get()) })
            }
            _ => None,
        };
        if let Some(window) = &window {
            window.reassert_overlay_style();
            let (x, y) = resolve_position(
                settings.overlay.x.zip(settings.overlay.y),
                ESTIMATED_SIZE,
                is_on_any_monitor,
            );
            window.move_to(x, y);
        }

        let controls = Controls::install(ctx, &settings.hotkeys);
        let mut notes: Vec<String> = notice.into_iter().collect();
        notes.extend(controls.hotkey_errors.iter().cloned());

        let repaint = ctx.clone();
        let engine = diag::timed("движок запущен", || {
            Engine::start(
                EngineConfig {
                    presentmon,
                    presentmon_override: settings.fps.presentmon_path.as_ref().map(PathBuf::from),
                    target: settings.fps.target_settings(),
                },
                move || repaint.request_repaint_of(egui::ViewportId::ROOT),
            )
        });

        let mut settings_window = SettingsWindow::default();
        // `mh-monitor --settings` — сразу с открытыми настройками (ярлык, проверка).
        if std::env::args().any(|arg| arg == "--settings") {
            // Как и из трея: без видимого оверлея окно настроек не рисуется.
            settings.overlay.visible = true;
            let position = window.as_ref().and_then(|window| {
                settings_position(window, cc.egui_ctx.pixels_per_point(), settings.overlay.scale)
            });
            settings_window.open(&settings, position);
        }

        Self {
            engine: Some(engine),
            saved: settings.clone(),
            settings,
            settings_path,
            save_due: None,
            settings_window,
            controls,
            window,
            applied: None,
            next_housekeeping: Instant::now(),
            hud_rect: None,
            last_size: None,
            seen_rows: SeenRows::new(),
            notes,
            logged_fps: String::new(),
            pixels_per_point: cc.egui_ctx.pixels_per_point(),
            exiting: false,
        }
    }

    fn apply(&mut self, command: Command) {
        diag::log(format!("команда: {command:?}"));
        let overlay = &mut self.settings.overlay;
        match command {
            Command::ToggleVisible => {
                overlay.visible = !overlay.visible;
                // Скрытый оверлей eframe не рисует — вместе с ним не рисуются и настройки.
                if !overlay.visible {
                    self.settings_window.open = false;
                }
            }
            Command::ToggleLock => overlay.locked = !overlay.locked,
            Command::OpenSettings => {
                overlay.visible = true;
                let position = self.window.as_ref().and_then(|window| {
                    settings_position(window, self.pixels_per_point, self.settings.overlay.scale)
                });
                self.settings_window.open(&self.settings, position);
            }
            Command::Exit => self.exiting = true,
        }
    }

    /// Доводит окно и движок до настроек. Вызывается каждый кадр; дёшево, пока ничего не менялось.
    fn sync(&mut self, ctx: &egui::Context) {
        let wanted = Applied::of(&self.settings);
        let previous = self.applied.replace(wanted.clone());
        if previous.as_ref() == Some(&wanted) {
            return;
        }
        let changed =
            |pick: fn(&Applied) -> bool| previous.as_ref().map(pick) != Some(pick(&wanted));

        if changed(|a| a.visible) {
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                ViewportCommand::Visible(wanted.visible),
            );
        }
        if changed(|a| a.locked) {
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                ViewportCommand::MousePassthrough(wanted.locked),
            );
        }
        if changed(|a| a.always_on_top) {
            let level =
                if wanted.always_on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal };
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, ViewportCommand::WindowLevel(level));
        }
        if previous.as_ref().map(|a| a.scale) != Some(wanted.scale) {
            ctx.set_zoom_factor(wanted.scale);
            self.last_size = None;
        }
        if previous.as_ref().map(|a| &a.target) != Some(&wanted.target)
            && let Some(engine) = &self.engine
        {
            engine.set_target(wanted.target.clone());
        }
        self.controls.sync_menu(wanted.visible, wanted.locked);
        // winit только что мог переписать расширенный стиль — вернуть наши биты при ближайшей уборке.
        self.next_housekeeping = Instant::now();
    }

    fn housekeeping(&mut self) {
        let now = Instant::now();
        if now < self.next_housekeeping {
            return;
        }
        self.next_housekeeping = now + HOUSEKEEPING;
        let Some(window) = self.window else { return };
        if !self.settings.overlay.visible {
            return;
        }
        window.reassert_overlay_style();
        if self.settings.overlay.always_on_top {
            window.bring_to_top();
        }

        let Some(rect) = window.rect() else { return };
        if !is_on_any_monitor(rect) {
            // Монитор отключили вместе с HUD.
            window.move_to(DEFAULT_POSITION.0, DEFAULT_POSITION.1);
            return;
        }
        self.settings.overlay.x = Some(rect.left);
        self.settings.overlay.y = Some(rect.top);
    }

    fn log_fps(&mut self, snapshot: &mh_core::Snapshot) {
        let fps = &snapshot.fps;
        let line = format!(
            "кадры: {:?}, {}, вывод: {}",
            fps.availability,
            fps.summary().unwrap_or_default(),
            fps.presentation.as_deref().unwrap_or("—")
        );
        if line != self.logged_fps {
            diag::log(&line);
            self.logged_fps = line;
        }
    }

    /// Пишет на диск, когда настройки изменились и успокоились.
    fn save_if_due(&mut self, force: bool) {
        let now = Instant::now();
        if self.settings != self.saved {
            if self.save_due.is_none() {
                self.save_due = Some(now + SAVE_DELAY);
            }
        } else {
            self.save_due = None;
        }
        let due = self.save_due.is_some_and(|at| force || now >= at);
        if due {
            // Не записалось — попробуем при следующем изменении; работать это не мешает.
            if self.settings.save(&self.settings_path).is_ok() {
                self.saved = self.settings.clone();
            }
            self.save_due = None;
        }
    }
}

impl eframe::App for OverlayApp {
    /// Вызывается и тогда, когда окно скрыто, — поэтому команды обрабатываются здесь.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pixels_per_point = ctx.pixels_per_point();
        while let Ok(command) = self.controls.commands.try_recv() {
            self.apply(command);
        }
        if ctx.input(|input| input.viewport().close_requested()) && !self.exiting {
            // Alt+F4 по оверлею прячет его, а не завершает: выход — из трея.
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.apply(Command::ToggleVisible);
        }
        self.sync(ctx);
        self.housekeeping();
        self.save_if_due(self.exiting);
        if self.exiting {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let snapshot = self.engine.as_ref().map(Engine::snapshot).unwrap_or_default();

        // Область перетаскивания — до содержимого, чтобы кнопки заголовка лежали поверх неё.
        if !self.settings.overlay.locked
            && let Some(rect) = self.hud_rect
            && ui.interact(rect, Id::new("hud-drag"), Sense::drag()).drag_started()
        {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }

        self.log_fps(&snapshot);

        let mut actions = HudActions::default();
        let rect = hud::show(
            ui,
            &snapshot,
            &self.settings,
            &mut self.seen_rows,
            &self.notes,
            &mut actions,
        );
        self.hud_rect = Some(rect);

        // Окно повторяет размер HUD: высота зависит от того, какие строки есть. Размер в точках
        // переводится в пиксели по текущему масштабу, поэтому масштаб — часть ключа: после его
        // смены тот же размер в точках — это другое окно.
        let size = rect.size();
        let pixels_per_point = ctx.pixels_per_point();
        let changed = self.last_size.is_none_or(|(last, last_ppp)| {
            (last - size).length() > 0.5 || (last_ppp - pixels_per_point).abs() > f32::EPSILON
        });
        if changed {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
            self.last_size = Some((size, pixels_per_point));
        }

        if actions.toggle_lock {
            self.apply(Command::ToggleLock);
        }
        if actions.hide {
            self.apply(Command::ToggleVisible);
        }

        let info = settings_window::Context {
            snapshot: &snapshot,
            settings_path: &self.settings_path,
            hotkey_errors: &self.controls.hotkey_errors,
        };
        self.settings_window.show(&ctx, &mut self.settings, &info);
        // Изменения из окна настроек доходят до окна оверлея уже в этом кадре.
        self.sync(&ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        diag::log("выход");
        diag::timed("настройки записаны", || self.save_if_due(true));
        // Остановка захвата — здесь, а не когда-нибудь в Drop: гарантий, что eframe уничтожит
        // приложение до выхода процесса, нет, а незакрытая ETW-сессия ломает захват всей машине.
        let engine = self.engine.take();
        diag::timed("движок остановлен", || drop(engine));
    }
}

/// Точка для окна настроек рядом с HUD, в точках egui.
fn settings_position(window: &OverlayWindow, pixels_per_point: f32, zoom: f32) -> Option<Pos2> {
    let hud = window.rect()?;
    let work = work_area_near(hud)?;
    // `pixels_per_point` уже включает масштаб HUD; экранный масштаб — без него.
    let screen_scale = (pixels_per_point / zoom.max(0.1)).max(0.1);
    let size = crate::settings_window::SIZE;
    let width = (size[0] * pixels_per_point).round() as i32;
    let height = (size[1] * pixels_per_point).round() as i32;
    let (x, y) = beside(hud, work, width, height);
    Some(pos2(x as f32 / screen_scale, y as f32 / screen_scale))
}

/// Левый верхний угол окна `width × height` рядом с `hud` внутри `work`, в физических пикселях.
///
/// Справа, если помещается, иначе слева; если нет места ни там, ни там — у правого края, поверх.
fn beside(hud: ScreenRect, work: ScreenRect, width: i32, height: i32) -> (i32, i32) {
    const GAP: i32 = 16;
    let right = hud.right + GAP;
    let left = hud.left - GAP - width;
    let x = if right + width <= work.right {
        right
    } else if left >= work.left {
        left
    } else {
        (work.right - width).max(work.left)
    };
    let y = hud.top.clamp(work.top, (work.bottom - height).max(work.top));
    (x, y)
}

/// Сохранённая позиция, если окно там видно хотя бы частично; иначе — место по умолчанию.
fn resolve_position(
    saved: Option<(i32, i32)>,
    (width, height): (i32, i32),
    on_monitor: impl Fn(ScreenRect) -> bool,
) -> (i32, i32) {
    match saved {
        Some((x, y))
            if on_monitor(ScreenRect { left: x, top: y, right: x + width, bottom: y + height }) =>
        {
            (x, y)
        }
        _ => DEFAULT_POSITION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_position_on_a_monitor_is_kept() {
        assert_eq!(resolve_position(Some((-1_800, 50)), (272, 420), |_| true), (-1_800, 50));
    }

    #[test]
    fn a_position_on_a_disconnected_monitor_falls_back() {
        assert_eq!(resolve_position(Some((5_000, 50)), (272, 420), |_| false), DEFAULT_POSITION);
    }

    #[test]
    fn no_saved_position_uses_the_default() {
        assert_eq!(resolve_position(None, (272, 420), |_| true), DEFAULT_POSITION);
    }

    const SCREEN: ScreenRect = ScreenRect { left: 0, top: 0, right: 2560, bottom: 1400 };

    #[test]
    fn settings_open_to_the_right_of_a_hud_on_the_left() {
        let hud = ScreenRect { left: 20, top: 20, right: 300, bottom: 500 };
        assert_eq!(beside(hud, SCREEN, 960, 750), (316, 20));
    }

    #[test]
    fn settings_open_to_the_left_of_a_hud_on_the_right() {
        let hud = ScreenRect { left: 2260, top: 900, right: 2540, bottom: 1380 };
        assert_eq!(beside(hud, SCREEN, 960, 750), (2260 - 16 - 960, 650));
    }

    #[test]
    fn a_narrow_screen_still_keeps_the_window_on_it() {
        let screen = ScreenRect { left: -1280, top: 0, right: 0, bottom: 1000 };
        let hud = ScreenRect { left: -900, top: 10, right: -620, bottom: 400 };
        let (x, y) = beside(hud, screen, 960, 750);
        assert!(x >= screen.left && x + 960 <= screen.right);
        assert_eq!(y, 10);
    }

    #[test]
    fn applied_state_follows_the_settings() {
        let mut settings = Settings::default();
        let before = Applied::of(&settings);
        settings.fps.target = settings::TargetChoice::Manual;
        settings.fps.manual_process = Some("dmc4.exe".into());
        let after = Applied::of(&settings);
        assert_ne!(before.target, after.target);
        assert_eq!(before.visible, after.visible);
    }
}

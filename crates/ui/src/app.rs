//! Окно оверлея: связывает движок, HUD, трей и хоткеи.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Id, Rect, Sense, Vec2, ViewportBuilder, ViewportCommand};
use mh_core::TargetSettings;
use mh_engine::{Engine, EngineConfig, extract_presentmon};
use mh_platform::overlay::{OverlayWindow, ScreenRect, is_on_any_monitor};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::controls::{Command, Controls};
use crate::hud::{self, HudActions};
use crate::settings::{self, Settings};
use crate::theme;

/// Как часто оверлей снова забирает верх z-порядка и проверяет своё место на экране.
const HOUSEKEEPING: Duration = Duration::from_secs(1);
/// Настройки пишутся не на каждое движение, а когда всё успокоилось.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// Куда ставить HUD, если сохранённой позиции нет или она на отключённом мониторе.
const DEFAULT_POSITION: (i32, i32) = (20, 20);
/// Оценка размера окна для проверки позиции до первого кадра, в физических пикселях.
const ESTIMATED_SIZE: (i32, i32) = (272, 420);

pub fn run() -> eframe::Result {
    let settings_path = settings::default_path();
    let settings = Settings::load(&settings_path);
    // Не разложился — PresentMon будет «не найден», и движок честно откатится на свой ETW.
    let presentmon = extract_presentmon(&settings::local_dir().join("bin"))
        .unwrap_or_else(|_| settings::local_dir().join("bin").join("missing-presentmon.exe"));

    let overlay = &settings.overlay;
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("MH Monitor")
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false)
            .with_inner_size([theme::OVERLAY_WIDTH, ESTIMATED_SIZE.1 as f32])
            .with_mouse_passthrough(overlay.locked)
            .with_visible(overlay.visible),
        persist_window: false,
        ..Default::default()
    };

    eframe::run_native(
        "MH Monitor",
        options,
        Box::new(move |cc| Ok(Box::new(OverlayApp::new(cc, settings, settings_path, presentmon)))),
    )
}

struct OverlayApp {
    /// `None` после выхода: движок останавливается явно, в `on_exit`.
    engine: Option<Engine>,
    settings: Settings,
    settings_path: PathBuf,
    save_due: Option<Instant>,
    controls: Controls,
    window: Option<OverlayWindow>,
    /// Что уже применено к окну: (видимость, блокировка).
    applied: Option<(bool, bool)>,
    next_housekeeping: Instant,
    /// Где HUD был в прошлом кадре — по этой области его таскают.
    hud_rect: Option<Rect>,
    last_size: Option<Vec2>,
    exiting: bool,
}

impl OverlayApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        settings_path: PathBuf,
        presentmon: PathBuf,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        ctx.set_fonts(theme::fonts());
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
        let repaint = ctx.clone();
        let engine = Engine::start(
            EngineConfig { presentmon, presentmon_override: None, target: TargetSettings::auto() },
            move || repaint.request_repaint(),
        );

        Self {
            engine: Some(engine),
            settings,
            settings_path,
            save_due: None,
            controls,
            window,
            applied: None,
            next_housekeeping: Instant::now(),
            hud_rect: None,
            last_size: None,
            exiting: false,
        }
    }

    fn settings_changed(&mut self) {
        self.save_due = Some(Instant::now() + SAVE_DELAY);
    }

    fn apply(&mut self, command: Command) {
        let overlay = &mut self.settings.overlay;
        match command {
            Command::ToggleVisible => overlay.visible = !overlay.visible,
            Command::ToggleLock => overlay.locked = !overlay.locked,
            Command::Exit => self.exiting = true,
        }
        self.settings_changed();
    }

    /// Доводит окно до состояния из настроек, если оно изменилось.
    fn sync_window(&mut self, ctx: &egui::Context) {
        let wanted = (self.settings.overlay.visible, self.settings.overlay.locked);
        if self.applied == Some(wanted) {
            return;
        }
        ctx.send_viewport_cmd(ViewportCommand::Visible(wanted.0));
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(wanted.1));
        self.controls.sync_menu(wanted.0, wanted.1);
        self.applied = Some(wanted);
        // winit только что переписал расширенный стиль — вернуть наши биты при ближайшей уборке.
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
        window.bring_to_top();

        let Some(rect) = window.rect() else { return };
        if !is_on_any_monitor(rect) {
            // Монитор отключили вместе с HUD.
            window.move_to(DEFAULT_POSITION.0, DEFAULT_POSITION.1);
            return;
        }
        let position = Some((rect.left, rect.top));
        if self.settings.overlay.x.zip(self.settings.overlay.y) != position {
            self.settings.overlay.x = Some(rect.left);
            self.settings.overlay.y = Some(rect.top);
            self.settings_changed();
        }
    }

    fn save_if_due(&mut self, force: bool) {
        let due = self.save_due.is_some_and(|at| force || Instant::now() >= at);
        if due {
            // Не записалось — попробуем при следующем изменении; работать это не мешает.
            let _ = self.settings.save(&self.settings_path);
            self.save_due = None;
        }
    }
}

impl eframe::App for OverlayApp {
    /// Вызывается и тогда, когда окно скрыто, — поэтому команды обрабатываются здесь.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(command) = self.controls.commands.try_recv() {
            self.apply(command);
        }
        if ctx.input(|input| input.viewport().close_requested()) && !self.exiting {
            // Alt+F4 по оверлею прячет его, а не завершает: выход — из трея.
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.settings.overlay.visible = false;
            self.settings_changed();
        }
        self.sync_window(ctx);
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

        let mut actions = HudActions::default();
        let rect = hud::show(
            ui,
            &snapshot,
            &self.settings.overlay,
            &self.controls.hotkey_errors,
            &mut actions,
        );
        self.hud_rect = Some(rect);

        // Окно повторяет размер HUD: высота зависит от того, какие строки есть.
        let size = rect.size();
        if self.last_size.is_none_or(|last| (last - size).length() > 0.5) {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
            self.last_size = Some(size);
        }

        if actions.toggle_lock {
            self.apply(Command::ToggleLock);
        }
        if actions.hide {
            self.apply(Command::ToggleVisible);
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_if_due(true);
        // Остановка захвата — здесь, а не когда-нибудь в Drop: гарантий, что eframe уничтожит
        // приложение до выхода процесса, нет, а незакрытая ETW-сессия ломает захват всей машине.
        drop(self.engine.take());
    }
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
}

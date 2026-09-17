//! Окно первого запуска (PLAN.md §6/P9).
//!
//! Раньше первый запуск открывал настройки на «Установке», и их можно было просто закрыть: FPS
//! не появлялся, а почему — знал только README. Теперь выбор нельзя пропустить, не сделав его:
//! установить службу, запуститься от администратора в этот раз или работать без FPS. Здесь же,
//! до установки, выбирается поведение HUD в эксклюзивном полноэкранном режиме.

use eframe::egui::{
    self, Button, RichText, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
    WindowLevel,
};

use crate::settings::{FullscreenMode, Settings};
use crate::settings_window::{group, hint, note, warning};
use crate::theme;

/// Что выбрал пользователь. Выполняет приложение: нужен UAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupChoice {
    /// Установка со службой: один запрос UAC, дальше без прав.
    Install,
    /// Перезапуск этой копии от администратора; в следующий раз окно спросит снова.
    ElevateOnce,
    /// Без прав: FPS и температуры CPU не будет.
    Skip,
}

/// Что окно показывает, но не меняет.
pub struct Context<'a> {
    /// Идёт установка или ожидание UAC.
    pub busy: bool,
    /// Итог последней попытки, если она не удалась.
    pub status: Option<&'a str>,
    pub monitors: usize,
}

const SIZE: [f32; 2] = [560.0, 470.0];

pub struct SetupWindow {
    pub open: bool,
    /// Флажок автозапуска — применяется вместе с выбором.
    pub autostart: bool,
    focus_pending: bool,
}

impl SetupWindow {
    pub fn new(open: bool) -> SetupWindow {
        SetupWindow { open, autostart: false, focus_pending: open }
    }

    /// Рисует окно, если оно открыто. Возвращает выбор, если кнопку нажали.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        settings: &mut Settings,
        info: &Context<'_>,
    ) -> Option<SetupChoice> {
        if !self.open {
            return None;
        }
        let builder = ViewportBuilder::default()
            .with_title("MH Monitoring — первый запуск")
            .with_inner_size(SIZE)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        ctx.show_viewport_immediate(ViewportId::from_hash_of("setup"), builder, |ui, class| {
            if class == ViewportClass::EmbeddedWindow {
                return None;
            }
            if std::mem::take(&mut self.focus_pending) {
                ui.ctx().send_viewport_cmd(ViewportCommand::Focus);
            }
            if ui.ctx().input(|input| input.viewport().close_requested()) {
                // Закрыли без выбора — спросим при следующем запуске.
                self.open = false;
            }
            let mut choice = None;
            egui::CentralPanel::default().show(ui, |ui| {
                choice = self.contents(ui, settings, info);
            });
            choice
        })
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        info: &Context<'_>,
    ) -> Option<SetupChoice> {
        ui.add_space(6.0);
        ui.label(
            RichText::new("Добро пожаловать в MH Monitoring")
                .font(theme::bold(20.0))
                .color(theme::TEXT_PRIMARY),
        );
        note(
            ui,
            "Видеокарта, загрузка CPU и память показываются сразу. Для FPS и температуры CPU \
             Windows требует прав администратора — выберите, как их дать.",
        );

        group(ui, "Полноэкранные игры");
        let mode = &mut settings.overlay.fullscreen;
        ui.radio_value(mode, FullscreenMode::Hide, "Скрывать HUD");
        ui.radio_value(mode, FullscreenMode::OtherMonitor, "Переносить HUD на другой монитор");
        hint(
            ui,
            "В эксклюзивном полноэкранном режиме окна поверх игры не видны. Лучше всего выбрать \
             в игре безрамочный режим — там HUD работает всегда. Можно поменять в настройках.",
        );
        if *mode == FullscreenMode::OtherMonitor && info.monitors < 2 {
            warning(ui, "Сейчас подключён один монитор — HUD будет скрываться.");
        }

        group(ui, "Права администратора");
        ui.checkbox(&mut self.autostart, "Запускать вместе с Windows");
        ui.add_space(6.0);
        let mut choice = None;
        ui.add_enabled_ui(!info.busy, |ui| {
            let wide = |text: &str| Button::new(RichText::new(text).font(theme::bold(15.0)));
            if ui.add(wide("Установить (рекомендуется)")).clicked() {
                choice = Some(SetupChoice::Install);
            }
            hint(
                ui,
                &format!(
                    "Один запрос UAC: программа копируется в {} и ставит службу. Дальше FPS и \
                     температура работают без прав администратора и без запросов.",
                    crate::installer::install_dir().display()
                ),
            );
            ui.add_space(6.0);
            if ui.button("Только сейчас — от администратора").clicked()
            {
                choice = Some(SetupChoice::ElevateOnce);
            }
            hint(ui, "Без установки. Запрос UAC будет при каждом запуске.");
            ui.add_space(6.0);
            if ui.button("Продолжить без FPS").clicked() {
                choice = Some(SetupChoice::Skip);
            }
            hint(ui, "Установить можно позже: настройки → «Установка».");
        });
        if info.busy {
            ui.add_space(6.0);
            note(ui, "Ожидание подтверждения UAC…");
        } else if let Some(status) = info.status {
            ui.add_space(6.0);
            warning(ui, status);
        }
        choice
    }
}

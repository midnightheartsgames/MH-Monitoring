//! Трей и глобальные хоткеи. Оба присылают команды в один канал.
//!
//! Обработчики событий будят UI: пока оверлей скрыт, eframe не рисует кадры, и без пробуждения
//! команду «показать» было бы некому выполнить.

use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::settings::HotkeySettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ToggleVisible,
    ToggleLock,
    Exit,
}

pub struct Controls {
    pub commands: Receiver<Command>,
    _tray: Option<TrayIcon>,
    visibility_item: MenuItem,
    lock_item: MenuItem,
    _hotkeys: Option<GlobalHotKeyManager>,
    /// Хоткеи, которые не удалось зарегистрировать, — для подсказки пользователю.
    pub hotkey_errors: Vec<String>,
}

impl Controls {
    pub fn install(ctx: &egui::Context, hotkeys: &HotkeySettings) -> Controls {
        let (sender, commands) = channel();

        let visibility_item = MenuItem::new("Скрыть оверлей", true, None);
        let lock_item = MenuItem::new("Заблокировать", true, None);
        let exit_item = MenuItem::new("Выход", true, None);
        let menu = Menu::new();
        let _ = menu.append_items(&[
            &visibility_item,
            &lock_item,
            &PredefinedMenuItem::separator(),
            &exit_item,
        ]);

        let ids = (visibility_item.id().clone(), lock_item.id().clone(), exit_item.id().clone());
        let menu_sender = sender.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let command = if event.id == ids.0 {
                Command::ToggleVisible
            } else if event.id == ids.1 {
                Command::ToggleLock
            } else if event.id == ids.2 {
                Command::Exit
            } else {
                return;
            };
            send(&menu_sender, &menu_ctx, command);
        }));

        // Левый клик по значку — показать или скрыть; меню — по правому.
        let tray_sender = sender.clone();
        let tray_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                send(&tray_sender, &tray_ctx, Command::ToggleVisible);
            }
        }));

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("MH Monitor")
            .with_icon(tray_icon())
            .build()
            .ok();

        let (manager, hotkey_errors) = register_hotkeys(ctx, &sender, hotkeys);

        Controls {
            commands,
            _tray: tray,
            visibility_item,
            lock_item,
            _hotkeys: manager,
            hotkey_errors,
        }
    }

    /// Подписи пунктов меню следуют состоянию оверлея.
    pub fn sync_menu(&self, visible: bool, locked: bool) {
        self.visibility_item.set_text(if visible {
            "Скрыть оверлей"
        } else {
            "Показать оверлей"
        });
        self.lock_item.set_text(if locked {
            "Разблокировать"
        } else {
            "Заблокировать"
        });
    }
}

fn send(sender: &Sender<Command>, ctx: &egui::Context, command: Command) {
    let _ = sender.send(command);
    ctx.request_repaint();
}

fn register_hotkeys(
    ctx: &egui::Context,
    sender: &Sender<Command>,
    settings: &HotkeySettings,
) -> (Option<GlobalHotKeyManager>, Vec<String>) {
    let mut errors = Vec::new();
    let Ok(manager) = GlobalHotKeyManager::new() else {
        errors.push("глобальные хоткеи недоступны".to_string());
        return (None, errors);
    };
    let mut bindings = Vec::new();
    for (text, command) in [
        (&settings.toggle_visibility, Command::ToggleVisible),
        (&settings.toggle_lock, Command::ToggleLock),
    ] {
        match parse_hotkey(text) {
            Some(hotkey) => match manager.register(hotkey) {
                Ok(()) => bindings.push((hotkey.id(), command)),
                // Обычно — занят другой программой.
                Err(_) => errors.push(format!("хоткей {text} занят другой программой")),
            },
            None => errors.push(format!("хоткей «{text}» не разобран")),
        }
    }
    let hotkey_sender = sender.clone();
    let hotkey_ctx = ctx.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state != HotKeyState::Pressed {
            return;
        }
        if let Some((_, command)) = bindings.iter().find(|(id, _)| *id == event.id) {
            send(&hotkey_sender, &hotkey_ctx, *command);
        }
    }));
    (Some(manager), errors)
}

/// «Ctrl+Shift+F11» → [`HotKey`]. Разбор global-hotkey понимает «ctrl», «shift», «alt».
pub fn parse_hotkey(text: &str) -> Option<HotKey> {
    text.parse().ok()
}

/// Значок трея: три столбика акцентного цвета, как у старого `TrayIconPainter`.
fn tray_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let bars = [(4, 18), (13, 8), (22, 13)]; // (x, верх столбика)
    for (left, top) in bars {
        for y in top..28 {
            for x in left..left + 6 {
                let offset = ((y * SIZE + x) * 4) as usize;
                rgba[offset..offset + 4].copy_from_slice(&[0x3F, 0xD0, 0xD8, 0xFF]);
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("размеры значка верны")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkeys_parse() {
        let defaults = HotkeySettings::default();
        let visibility = parse_hotkey(&defaults.toggle_visibility).expect("Ctrl+Shift+F11");
        let lock = parse_hotkey(&defaults.toggle_lock).expect("Ctrl+Shift+F10");
        assert_ne!(visibility.id(), lock.id());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_hotkey("Ctrl+Nonsense").is_none());
        assert!(parse_hotkey("").is_none());
    }
}

//! Цикл сообщений для служебного потока.
//!
//! Трей и глобальные хоткеи создают скрытые окна в своём потоке, и сообщения им нужно
//! доставлять. В потоке winit этого делать нельзя: модальный цикл меню трея (`TrackPopupMenu`)
//! внутри цикла winit подвешивает его — HUD замирал до первого движения мыши над ним, а команды
//! «Настройки» и «Выход» ждали вместе с ним (PLAN.md §2.16).

use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, QS_ALLINPUT, TranslateMessage,
};

/// Ждёт сообщений не дольше `wait_ms` и разбирает всё, что пришло в очередь этого потока.
pub fn pump_messages(wait_ms: u32) {
    unsafe {
        MsgWaitForMultipleObjectsEx(0, std::ptr::null(), wait_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_queue_returns_after_the_wait() {
        let started = std::time::Instant::now();
        pump_messages(30);
        let elapsed = started.elapsed();
        assert!(elapsed < std::time::Duration::from_millis(500), "{elapsed:?}");
    }
}

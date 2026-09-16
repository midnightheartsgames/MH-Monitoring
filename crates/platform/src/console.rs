//! Обработчик Ctrl+C и закрытия консоли.
//!
//! При Ctrl+C `Drop` ещё успевает отработать, если программа сама выйдет из цикла, — для этого
//! есть флаг [`stop_requested`]. А при закрытии окна консоли Windows завершает процесс вскоре
//! после возврата из обработчика, и до `Drop` дело не доходит. Поэтому сессия гасится **прямо
//! в обработчике**, по имени: брошенная сессия ломает захват кадров на всей машине (PLAN.md §2.1).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};

use crate::etw::stop_by_name;

static SESSION_NAME: OnceLock<String> = OnceLock::new();
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ставит обработчик. `session_name` — имя собственной ETW-сессии приложения.
pub fn install_ctrl_handler(session_name: &str) -> bool {
    let _ = SESSION_NAME.set(session_name.to_string());
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
}

/// Пользователь попросил остановиться.
pub fn stop_requested() -> bool {
    STOP_REQUESTED.load(Ordering::SeqCst)
}

unsafe extern "system" fn handler(ctrl_type: u32) -> windows_sys::core::BOOL {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(name) = SESSION_NAME.get() {
        stop_by_name(name);
    }
    // Ctrl+C и Ctrl+Break обработаны: программа сама выйдет из цикла и приберётся. Остальное
    // (закрытие окна, выход из системы) передаём дальше — процесс всё равно завершат.
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => 1,
        _ => 0,
    }
}

//! Окно игры без рамки на весь монитор (PLAN.md §6/P10).
//!
//! Меняется только стиль чужого окна снаружи — `SetWindowLongPtrW` и `SetWindowPos`, как у
//! утилит вроде Borderless Gaming. В процесс игры ничего не попадает. Окно процесса с более
//! высоким уровнем целостности (игра от администратора) Windows менять не даст: это ошибка
//! `ERROR_ACCESS_DENIED`, её показывает UI.

use std::io;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT, SetLastError};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetClassNameW, GetClientRect,
    GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
    SetWindowLongPtrW, SetWindowPos, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME,
    WS_EX_STATICEDGE, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};

use crate::sys::from_wide;

/// Окно меньше этого — заставка или диалог, а не окно игры.
const MIN_SIZE: (i32, i32) = (320, 200);

/// Окно на переднем плане и его процесс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForegroundWindow {
    pub hwnd: isize,
    pub pid: u32,
}

pub fn foreground() -> Option<ForegroundWindow> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    (pid != 0).then_some(ForegroundWindow { hwnd: hwnd as isize, pid })
}

/// Чем кончилась попытка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Borderless {
    /// Рамка снята, окно растянуто.
    Applied,
    /// Уже без рамки и на весь монитор.
    AlreadyDone,
    /// Не окно игры: невидимое, дочернее, диалог или слишком маленькое.
    NotAGameWindow,
}

/// Стили без рамки и заголовка. Остальные биты не трогаются.
pub fn borderless_styles(style: u32, ex_style: u32) -> (u32, u32) {
    let style =
        style & !(WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU);
    let ex_style =
        ex_style & !(WS_EX_DLGMODALFRAME | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE | WS_EX_WINDOWEDGE);
    (style, ex_style)
}

/// Снимает рамку с окна `hwnd` и растягивает его на монитор, где оно стоит.
pub fn make_borderless(hwnd: isize) -> io::Result<Borderless> {
    let hwnd = hwnd as HWND;
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return Ok(Borderless::NotAGameWindow);
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let Some(rect) = window_rect(hwnd) else { return Ok(Borderless::NotAGameWindow) };
        let too_small = rect.right - rect.left < MIN_SIZE.0 || rect.bottom - rect.top < MIN_SIZE.1;
        if style & WS_CHILD != 0 || too_small || class_name(hwnd) == "#32770" {
            return Ok(Borderless::NotAGameWindow);
        }
        let Some(monitor) = monitor_rect(hwnd) else { return Ok(Borderless::NotAGameWindow) };

        let (new_style, new_ex_style) = borderless_styles(style, ex_style);
        let fits = rect.left == monitor.left
            && rect.top == monitor.top
            && rect.right == monitor.right
            && rect.bottom == monitor.bottom;
        if new_style == style && new_ex_style == ex_style && fits {
            return Ok(Borderless::AlreadyDone);
        }
        set_long(hwnd, GWL_STYLE, new_style)?;
        set_long(hwnd, GWL_EXSTYLE, new_ex_style)?;
        let ok = SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            monitor.left,
            monitor.top,
            monitor.right - monitor.left,
            monitor.bottom - monitor.top,
            SWP_FRAMECHANGED | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE,
        );
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Borderless::Applied)
    }
}

/// `SetWindowLongPtrW` возвращает прежнее значение, и 0 — не всегда ошибка.
/// Размер клиентской области главного окна процесса, в физических пикселях: самое большое
/// видимое окно верхнего уровня без владельца. Для игры это разрешение, в котором она рисует.
///
/// `None` — видимых окон у процесса нет (свёрнута, ещё грузится или это не он рисует).
pub fn client_size(pid: u32) -> Option<(u32, u32)> {
    struct Search {
        pid: u32,
        best: Option<(u32, u32)>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> windows_sys::core::BOOL {
        // SAFETY: `data` — указатель на `Search` из `client_size`, живой на время перечисления.
        let search = unsafe { &mut *(data as *mut Search) };
        let mut owner_pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner_pid) };
        let candidate = owner_pid == search.pid
            && unsafe { IsWindowVisible(hwnd) } != 0
            && unsafe { GetWindow(hwnd, GW_OWNER) }.is_null();
        if candidate {
            let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            if unsafe { GetClientRect(hwnd, &mut rect) } != 0 {
                let size = (
                    (rect.right - rect.left).max(0) as u32,
                    (rect.bottom - rect.top).max(0) as u32,
                );
                let area = |(width, height): (u32, u32)| u64::from(width) * u64::from(height);
                if size.0 > 0
                    && size.1 > 0
                    && search.best.is_none_or(|best| area(size) > area(best))
                {
                    search.best = Some(size);
                }
            }
        }
        1
    }
    if pid == 0 {
        return None;
    }
    let mut search = Search { pid, best: None };
    unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
    search.best
}

unsafe fn set_long(hwnd: HWND, index: i32, value: u32) -> io::Result<()> {
    unsafe {
        SetLastError(0);
        let previous = SetWindowLongPtrW(hwnd, index, value as isize);
        let error = io::Error::last_os_error();
        if previous == 0 && error.raw_os_error() != Some(0) {
            return Err(error);
        }
    }
    Ok(())
}

unsafe fn window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    (unsafe { GetWindowRect(hwnd, &mut rect) } != 0).then_some(rect)
}

unsafe fn monitor_rect(hwnd: HWND) -> Option<RECT> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then_some(info.rcMonitor)
}

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 128];
    let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    from_wide(&buffer[..length.max(0) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        WS_CLIPCHILDREN, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
    };

    #[test]
    fn a_normal_window_loses_its_frame_and_keeps_the_rest() {
        let (style, ex_style) = borderless_styles(
            WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_CLIPCHILDREN,
            WS_EX_WINDOWEDGE | WS_EX_CLIENTEDGE | WS_EX_APPWINDOW,
        );
        assert_eq!(style, WS_VISIBLE | WS_CLIPCHILDREN);
        assert_eq!(ex_style, WS_EX_APPWINDOW, "окно остаётся на панели задач");
    }

    #[test]
    fn a_popup_window_is_already_borderless() {
        let style = WS_POPUP | WS_VISIBLE;
        assert_eq!(borderless_styles(style, 0), (style, 0));
    }

    #[test]
    fn a_process_without_windows_has_no_size() {
        // У тестового процесса окон нет; у несуществующего — тем более.
        assert_eq!(client_size(std::process::id()), None);
        assert_eq!(client_size(0), None);
    }

    #[test]
    fn some_window_is_in_the_foreground_or_none_is() {
        // На машине сборки окна на переднем плане может не быть; вызов не должен падать.
        if let Some(window) = foreground() {
            assert!(window.pid > 0);
        }
    }
}

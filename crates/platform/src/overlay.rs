//! Окно оверлея: стили, topmost, позиция (PLAN.md §6/P4).
//!
//! Прозрачность для мыши (`WS_EX_TRANSPARENT` + `WS_EX_LAYERED`) здесь **не** ставится: её ставит
//! winit, и только он при этом вызывает `SetLayeredWindowAttributes`. Layered-окно без этого
//! вызова не рисуется вовсе. Зато winit при каждом таком переключении переписывает расширенный
//! стиль целиком и стирает наши биты, поэтому [`OverlayWindow::reassert_overlay_style`] вызывается
//! регулярно.
//!
//! Все координаты — физические пиксели экрана. Логические точки egui при разном DPI мониторов
//! переводились бы по масштабу не того монитора.

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// Прямоугольник окна в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Окно оверлея по его `HWND` из raw-window-handle.
#[derive(Debug, Clone, Copy)]
pub struct OverlayWindow {
    hwnd: HWND,
}

// HWND — номер объекта ядра, а не указатель в нашу память; функции ниже потокобезопасны.
unsafe impl Send for OverlayWindow {}

impl OverlayWindow {
    /// # Safety
    /// `hwnd` — действующее окно этого процесса.
    pub unsafe fn from_raw(hwnd: isize) -> Self {
        Self { hwnd: hwnd as HWND }
    }

    /// Не в Alt+Tab и не на панели задач; клик и подъём не отбирают фокус у игры.
    ///
    /// `WS_EX_APPWINDOW` снимается: winit ставит его сам, а он возвращает окно в Alt+Tab даже
    /// при `WS_EX_TOOLWINDOW` (проверено на живом окне: `0x8040198`).
    ///
    /// Идемпотентно и дёшево: стиль пишется, только если бит пропал.
    pub fn reassert_overlay_style(&self) {
        unsafe {
            let current = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let wanted = (current | (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as isize)
                & !(WS_EX_APPWINDOW as isize);
            if wanted != current {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, wanted);
            }
        }
    }

    /// Снова наверх среди topmost-окон.
    ///
    /// Игра, уходящая в безрамочный полноэкранный режим, поднимает себя над всеми topmost-окнами —
    /// ровно тогда HUD и пропадает. Раз в секунду это ничего не стоит.
    pub fn bring_to_top(&self) {
        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    pub fn rect(&self) -> Option<ScreenRect> {
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if unsafe { GetWindowRect(self.hwnd, &mut rect) } == 0 {
            return None;
        }
        Some(ScreenRect { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom })
    }

    pub fn move_to(&self, x: i32, y: i32) {
        unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }
}

/// Пересекается ли прямоугольник хоть с одним подключённым монитором.
///
/// Сохранённая позиция с отключённого монитора иначе увела бы HUD за пределы экранов.
pub fn is_on_any_monitor(rect: ScreenRect) -> bool {
    let rect = RECT { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
    !unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_null()
}

/// Рабочая область (без панели задач) монитора, на котором больше всего от `rect`.
pub fn work_area_near(rect: ScreenRect) -> Option<ScreenRect> {
    let rect = RECT { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
    let monitor = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) };
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    let work = info.rcWork;
    Some(ScreenRect { left: work.left, top: work.top, right: work.right, bottom: work.bottom })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_monitor_origin_is_visible_and_far_space_is_not() {
        assert!(is_on_any_monitor(ScreenRect { left: 10, top: 10, right: 50, bottom: 50 }));
        let far =
            ScreenRect { left: -1_000_000, top: -1_000_000, right: -999_000, bottom: -999_000 };
        assert!(!is_on_any_monitor(far));
    }

    #[test]
    fn the_primary_monitor_has_a_work_area() {
        let area = work_area_near(ScreenRect { left: 10, top: 10, right: 50, bottom: 50 }).unwrap();
        assert!(area.right > area.left && area.bottom > area.top);
    }
}

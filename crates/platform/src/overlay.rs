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

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect, MonitorFromWindow,
};
use windows_sys::Win32::UI::Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST,
    MONITORINFOF_PRIMARY, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    SetWindowLongPtrW, SetWindowPos, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// Работает ли сейчас приложение Direct3D в эксклюзивном полноэкранном режиме.
///
/// Поверх такого режима окно пользовательского режима не видно. Хуже того, окно поверх игры
/// делает её «перекрытой», и многие игры перестают рисовать: кадр замирает, а при запуске сразу в
/// этом режиме экран остаётся чёрным. Поэтому оверлей на это время прячется (PLAN.md §2.16).
///
/// Состояние сообщает оболочка — тот же признак, по которому Windows откладывает уведомления.
/// Безрамочный режим сюда не попадает: для него оболочка отвечает `QUNS_BUSY`.
pub fn exclusive_fullscreen_active() -> bool {
    let mut state = 0;
    let result = unsafe { SHQueryUserNotificationState(&mut state) };
    result >= 0 && state == QUNS_RUNNING_D3D_FULL_SCREEN
}

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

/// Монитор: весь прямоугольник, рабочая область, основной ли.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub bounds: ScreenRect,
    pub work: ScreenRect,
    pub primary: bool,
}

fn screen_rect(rect: RECT) -> ScreenRect {
    ScreenRect { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom }
}

fn monitor_info(monitor: HMONITOR) -> Option<Monitor> {
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    Some(Monitor {
        bounds: screen_rect(info.rcMonitor),
        work: screen_rect(info.rcWork),
        primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

/// Все подключённые мониторы.
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> windows_sys::core::BOOL {
        // SAFETY: `data` — указатель на вектор из `monitors`, живой на время перечисления.
        let list = unsafe { &mut *(data as *mut Vec<Monitor>) };
        list.extend(monitor_info(monitor));
        1
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            &mut list as *mut Vec<Monitor> as LPARAM,
        );
    }
    list
}

/// Монитор окна на переднем плане — в эксклюзивном полноэкранном режиме это монитор игры.
pub fn foreground_monitor() -> Option<Monitor> {
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return None;
    }
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONULL) };
    if monitor.is_null() {
        return None;
    }
    monitor_info(monitor)
}

/// Рабочая область монитора, на котором нет игры: основной, если игра не на нём, иначе первый
/// подходящий. `None` — другого монитора нет.
pub fn other_work_area(monitors: &[Monitor], game: ScreenRect) -> Option<ScreenRect> {
    let others = monitors.iter().filter(|monitor| !intersects(monitor.bounds, game));
    others.clone().find(|monitor| monitor.primary).or_else(|| others.clone().next()).map(|m| m.work)
}

fn intersects(a: ScreenRect, b: ScreenRect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
        ScreenRect { left, top, right, bottom }
    }

    fn monitor(bounds: ScreenRect, primary: bool) -> Monitor {
        let work = ScreenRect { bottom: bounds.bottom - 40, ..bounds };
        Monitor { bounds, work, primary }
    }

    #[test]
    fn the_hud_goes_to_the_primary_monitor_when_the_game_is_elsewhere() {
        let main = monitor(rect(0, 0, 2560, 1440), true);
        let left = monitor(rect(-1920, 0, 0, 1080), false);
        let right = monitor(rect(2560, 0, 4480, 1080), false);
        assert_eq!(other_work_area(&[left, main, right], right.bounds), Some(main.work));
    }

    #[test]
    fn the_hud_leaves_the_primary_monitor_when_the_game_is_there() {
        let main = monitor(rect(0, 0, 2560, 1440), true);
        let side = monitor(rect(-1920, 0, 0, 1080), false);
        assert_eq!(other_work_area(&[main, side], main.bounds), Some(side.work));
    }

    #[test]
    fn a_single_monitor_has_no_other_place() {
        let main = monitor(rect(0, 0, 2560, 1440), true);
        assert_eq!(other_work_area(&[main], main.bounds), None);
    }

    #[test]
    fn this_machine_lists_at_least_one_monitor() {
        let list = monitors();
        assert!(!list.is_empty());
        assert!(list.iter().any(|monitor| monitor.primary));
    }

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

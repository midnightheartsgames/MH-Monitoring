//! Выбор файла игры (PLAN.md §6/P10).
//!
//! Системный диалог `IFileOpenDialog`. Окно настроек висит поверх всех окон, и диалог без
//! владельца открылся бы под ним. Поэтому владелец — скрытое topmost-окно, созданное в том же
//! потоке: у окна, которым владеет topmost-окно, тоже topmost. Окно чужого потока владельцем не
//! берётся, это связало бы очереди ввода потоков (см. PLAN.md §2.16 про меню трея).

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PCWSTR};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::sys::wide;

/// Показывает диалог выбора `.exe` и ждёт ответа. `None` — отменили или диалог не открылся.
///
/// Блокирует: звать из своего потока, не из потока окна.
pub fn pick_executable(title: &str) -> Option<PathBuf> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let class = wide("STATIC");
        let owner = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        let result = show(title, HWND(owner));
        if !owner.is_null() {
            DestroyWindow(owner);
        }
        if initialized {
            CoUninitialize();
        }
        result
    }
}

unsafe fn show(title: &str, owner: HWND) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let name = HSTRING::from("Программы (*.exe)");
        let pattern = HSTRING::from("*.exe");
        let filters = [COMDLG_FILTERSPEC {
            pszName: PCWSTR(name.as_ptr()),
            pszSpec: PCWSTR(pattern.as_ptr()),
        }];
        dialog.SetFileTypes(&filters).ok()?;
        dialog.SetTitle(&HSTRING::from(title)).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM).ok()?;
        let owner = (!owner.0.is_null()).then_some(owner);
        // Отмена — тоже ошибка (`ERROR_CANCELLED`).
        dialog.Show(owner).ok()?;
        let item = dialog.GetResult().ok()?;
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(PathBuf::from)
    }
}

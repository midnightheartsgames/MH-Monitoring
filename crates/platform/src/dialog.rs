//! Системные диалоги файлов: выбор игры (PLAN.md §6/P10), выгрузка и загрузка настроек.
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
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FileOpenDialog, FileSaveDialog,
    IFileDialog, IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, PCWSTR};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::sys::wide;

/// Фильтр диалога: подпись и маска, например «Программы (*.exe)» и `*.exe`.
#[derive(Debug, Clone, Copy)]
pub struct FileFilter<'a> {
    pub name: &'a str,
    pub pattern: &'a str,
}

const EXECUTABLES: FileFilter<'static> =
    FileFilter { name: "Программы (*.exe)", pattern: "*.exe" };

/// Показывает диалог выбора `.exe` и ждёт ответа. `None` — отменили или диалог не открылся.
///
/// Блокирует: звать из своего потока, не из потока окна.
pub fn pick_executable(title: &str) -> Option<PathBuf> {
    pick_file(title, EXECUTABLES)
}

/// Диалог открытия существующего файла. Блокирует, как и [`pick_executable`].
pub fn pick_file(title: &str, filter: FileFilter<'_>) -> Option<PathBuf> {
    with_owner(|owner| unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        configure(&dialog, title, filter, FOS_FILEMUSTEXIST)?;
        run(&dialog, owner)
    })
}

/// Диалог «Сохранить как»: `file_name` — предложенное имя. Перезапись спрашивает сам диалог.
pub fn pick_save_path(title: &str, file_name: &str, filter: FileFilter<'_>) -> Option<PathBuf> {
    with_owner(|owner| unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        configure(&dialog, title, filter, FOS_OVERWRITEPROMPT)?;
        dialog.SetFileName(&HSTRING::from(file_name)).ok()?;
        let extension = filter.pattern.trim_start_matches("*.");
        dialog.SetDefaultExtension(&HSTRING::from(extension)).ok()?;
        run(&dialog, owner)
    })
}

/// COM на время диалога и владелец — скрытое topmost-окно этого потока.
fn with_owner(show: impl FnOnce(HWND) -> Option<PathBuf>) -> Option<PathBuf> {
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
        let result = show(HWND(owner));
        if !owner.is_null() {
            DestroyWindow(owner);
        }
        if initialized {
            CoUninitialize();
        }
        result
    }
}

unsafe fn configure(
    dialog: &IFileDialog,
    title: &str,
    filter: FileFilter<'_>,
    extra: windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS,
) -> Option<()> {
    unsafe {
        let name = HSTRING::from(filter.name);
        let pattern = HSTRING::from(filter.pattern);
        let filters = [COMDLG_FILTERSPEC {
            pszName: PCWSTR(name.as_ptr()),
            pszSpec: PCWSTR(pattern.as_ptr()),
        }];
        dialog.SetFileTypes(&filters).ok()?;
        dialog.SetTitle(&HSTRING::from(title)).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | extra | FOS_FORCEFILESYSTEM).ok()
    }
}

unsafe fn run(dialog: &IFileDialog, owner: HWND) -> Option<PathBuf> {
    unsafe {
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

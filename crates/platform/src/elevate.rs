//! Запуск себя с правами администратора — для установки службы (PLAN.md §6/P6) и для работы
//! без службы «только в этот раз» (§6/P9).

use std::io;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};

use crate::sys::wide;

/// Запускает `executable` с аргументами через UAC и ждёт завершения. Возвращает код выхода.
///
/// Отказ пользователя в окне UAC — ошибка `ERROR_CANCELLED`.
pub fn run_elevated(executable: &Path, arguments: &str) -> io::Result<u32> {
    let verb = wide("runas");
    let file = wide(&executable.to_string_lossy());
    let parameters = wide(arguments);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_HIDE;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = info.hProcess;
    if process.is_null() {
        return Ok(0);
    }
    let mut code = 1u32;
    unsafe {
        if WaitForSingleObject(process, INFINITE) == WAIT_OBJECT_0 {
            GetExitCodeProcess(process, &mut code);
        }
        CloseHandle(process);
    }
    Ok(code)
}

/// Запускает `executable` через UAC и не ждёт его. Отказ в окне UAC — `ERROR_CANCELLED`.
pub fn launch_elevated(executable: &Path, arguments: &str) -> io::Result<()> {
    let verb = wide("runas");
    let file = wide(&executable.to_string_lossy());
    let parameters = wide(arguments);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

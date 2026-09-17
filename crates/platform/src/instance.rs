//! Экземпляры приложения: один оверлей, права процесса, копии установленного файла.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    TerminateProcess,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDNO, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNOCANCEL, MessageBoxW,
};

use crate::sys::wide;

/// Держит именованный мьютекс, пока жив.
pub struct SingleInstance {
    handle: HANDLE,
}

unsafe impl Send for SingleInstance {}

impl SingleInstance {
    /// `None` — другой экземпляр уже держит мьютекс и за `wait` его не отпустил.
    ///
    /// Ожидание нужно после установки: старая копия запускает установленную и только потом
    /// выходит.
    pub fn acquire(name: &str, wait: Duration) -> Option<SingleInstance> {
        let name_w = wide(name);
        let deadline = Instant::now() + wait;
        loop {
            let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name_w.as_ptr()) };
            if handle.is_null() {
                return None;
            }
            if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
                return Some(SingleInstance { handle });
            }
            unsafe { CloseHandle(handle) };
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Запущен ли процесс с правами администратора.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Полный путь к образу процесса, если его можно узнать.
pub fn image_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 32_768];
        let mut size = buffer.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size);
        CloseHandle(process);
        (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize])))
    }
}

/// Процессы, запущенные из этого файла, кроме текущего.
pub fn processes_from(image: &Path) -> Vec<u32> {
    let own = std::process::id();
    let wanted = image.to_string_lossy().to_lowercase();
    let mut result = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return result;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let pid = entry.th32ProcessID;
            if pid != own
                && image_path(pid)
                    .is_some_and(|path| path.to_string_lossy().to_lowercase() == wanted)
            {
                result.push(pid);
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    result
}

pub fn terminate(pid: u32) -> io::Result<()> {
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let ok = TerminateProcess(process, 0);
        CloseHandle(process);
        if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }
}

/// Сообщение пользователю, когда окна нет — например, после удаления.
pub fn message(title: &str, text: &str) {
    let title_w = wide(title);
    let text_w = wide(text);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        )
    };
}

/// Ответ на вопрос «Да / Нет / Отмена».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Cancel,
}

/// Окно с вопросом. Закрытие крестиком — `Cancel`.
pub fn ask(title: &str, text: &str) -> Answer {
    let title_w = wide(title);
    let text_w = wide(text);
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            title_w.as_ptr(),
            MB_YESNOCANCEL | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
    match result {
        IDYES => Answer::Yes,
        IDNO => Answer::No,
        _ => Answer::Cancel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_instance_is_refused_until_the_first_leaves() {
        let name = format!(r"Local\MHMonitor-test-{}", std::process::id());
        let first = SingleInstance::acquire(&name, Duration::ZERO).expect("первый");
        // Мьютекс рекурсивен для своего потока — второй экземпляр пробуем из другого.
        let other = name.clone();
        let refused = std::thread::spawn(move || {
            SingleInstance::acquire(&other, Duration::from_millis(150)).is_none()
        });
        assert!(refused.join().unwrap());
        drop(first);
        let other = name.clone();
        let taken =
            std::thread::spawn(move || SingleInstance::acquire(&other, Duration::ZERO).is_some());
        assert!(taken.join().unwrap());
    }

    #[test]
    fn this_process_knows_its_image() {
        let own = image_path(std::process::id()).unwrap();
        assert_eq!(own, std::env::current_exe().unwrap());
        // Себя в списке копий нет.
        assert!(!processes_from(&own).contains(&std::process::id()));
    }
}

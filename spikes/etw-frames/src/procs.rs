//! Поиск целевого процесса: по PID, по имени и по окну в фокусе.
//!
//! Для матрицы покрытия это не украшение: выяснять PID игры вручную перед каждым из пяти
//! замеров — и есть тот самый способ сделать замер в неподходящий момент.

use std::collections::HashMap;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};

use crate::sys::from_wide;

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
}

pub fn list_processes() -> Vec<ProcessInfo> {
    let mut result = Vec::new();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return result;
    }
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) };
    while ok != 0 {
        result.push(ProcessInfo { pid: entry.th32ProcessID, name: from_wide(&entry.szExeFile) });
        ok = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe { CloseHandle(snapshot) };
    result
}

pub fn name_by_pid() -> HashMap<u32, String> {
    list_processes().into_iter().map(|p| (p.pid, p.name)).collect()
}

pub fn find_by_name(needle: &str) -> Vec<ProcessInfo> {
    let needle = needle.to_lowercase();
    list_processes()
        .into_iter()
        .filter(|p| p.name.to_lowercase().contains(&needle))
        .collect()
}

/// Процесс окна, которое сейчас в фокусе, плюс заголовок окна.
///
/// Используется с задержкой: пользователь запускает спайк, переключается в игру, и цель
/// определяется уже по ней.
pub fn foreground_process() -> Option<(ProcessInfo, String)> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    let mut title = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    let title = from_wide(&title[..len.max(0) as usize]);
    let name = name_by_pid().get(&pid).cloned().unwrap_or_else(|| "?".to_string());
    Some((ProcessInfo { pid, name }, title))
}

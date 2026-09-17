//! Процессы: окно в фокусе, поиск по имени и PID, проверка «жив ли тот же запуск».
//!
//! Перенесено из `ForegroundProcessDetector.kt`. Решения — что считать оболочкой, когда менять
//! цель, как долго ждать — принимает `mh_core::target`; здесь только ответы Windows.

use mh_core::{ProcessLookup, TargetProcess, is_shell_process};
use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::sys::from_wide;

/// Дескриптор процесса, закрывающийся сам.
///
/// За полсотни смен цели незакрытые дескрипторы накопились бы полсотней — ровно то, что §8
/// требует проверять.
struct ProcessHandle(HANDLE);

impl ProcessHandle {
    /// `PROCESS_QUERY_LIMITED_INFORMATION` — минимальные права, которые Windows выдаёт и на
    /// процессы других пользователей, и на защищённые; `SYNCHRONIZE` — для проверки, жив ли он.
    fn open(process_id: u32) -> Option<Self> {
        if process_id == 0 {
            return None;
        }
        let handle = unsafe {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, process_id)
        };
        if handle.is_null() { None } else { Some(Self(handle)) }
    }

    fn image_name(&self) -> Option<String> {
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(self.0, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
        };
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
        if name.is_empty() { None } else { Some(name) }
    }

    /// Время создания процесса в миллисекундах от 1601 года — метка идентичности запуска.
    fn started_at_ms(&self) -> Option<u64> {
        let mut creation = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let mut unused = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 3];
        let ok = unsafe {
            GetProcessTimes(self.0, &mut creation, &mut unused[0], &mut unused[1], &mut unused[2])
        };
        if ok == 0 {
            return None;
        }
        let ticks = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        // FILETIME — сотни наносекунд.
        if ticks == 0 { None } else { Some(ticks / 10_000) }
    }

    /// Жив ли процесс.
    ///
    /// Не `GetExitCodeProcess` со сравнением с `STILL_ACTIVE`: процесс вправе завершиться с
    /// кодом 259 и выглядеть живым. Ожидание с нулевым таймаутом отвечает однозначно.
    fn is_running(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_TIMEOUT }
    }

    /// Частный рабочий набор — то, что диспетчер задач показывает в колонке «Память». Старые
    /// системы (до 1809) этого поля не заполняют, тогда отдаём весь рабочий набор.
    fn memory_bytes(&self) -> Option<u64> {
        let mut counters: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { std::mem::zeroed() };
        let size = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        counters.cb = size;
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                self.0,
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX2)
                    .cast::<PROCESS_MEMORY_COUNTERS>(),
                size,
            )
        };
        if ok == 0 {
            return None;
        }
        let bytes = if counters.PrivateWorkingSetSize > 0 {
            counters.PrivateWorkingSetSize
        } else {
            counters.WorkingSetSize
        };
        (bytes > 0).then_some(bytes as u64)
    }

    fn describe(&self, process_id: u32) -> Option<TargetProcess> {
        Some(TargetProcess::new(process_id, self.image_name()?, self.started_at_ms()))
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Процесс окна в фокусе — кандидат в цели.
///
/// `None`, когда в фокусе оболочка, собственное окно приложения или ничего. Для политики это
/// «кандидата нет», а вовсе не «цель пропала»: так Alt+Tab не сбрасывает замер.
///
/// Это определитель фокуса, а не игры: текстовый редактор он от игры не отличит. Что делать с
/// кандидатом, решает `mh_core::TargetTracker`.
pub fn foreground_process(own_process_id: u32) -> Option<TargetProcess> {
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return None;
    }
    let mut process_id = 0u32;
    unsafe { GetWindowThreadProcessId(window, &mut process_id) };
    if process_id == 0 || process_id == own_process_id {
        return None;
    }
    let target = ProcessHandle::open(process_id)?.describe(process_id)?;
    if is_shell_process(&target.executable) { None } else { Some(target) }
}

/// Все процессы с их PID и именами образов.
///
/// Снимок Toolhelp, а не `EnumProcesses`: он отдаёт имена без открытия каждого процесса, а
/// открыть удаётся далеко не каждый.
pub fn list_processes() -> Vec<(u32, String)> {
    let mut result = Vec::new();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return result;
    }
    let snapshot = ProcessHandle(snapshot);

    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while more {
        result.push((entry.th32ProcessID, from_wide(&entry.szExeFile)));
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    result
}

/// Сколько памяти занимает именно этот запуск процесса. `None`, если процесс завершился, его PID
/// достался другому или Windows не дала его открыть.
pub fn process_memory(target: &TargetProcess) -> Option<u64> {
    let handle = ProcessHandle::open(target.pid)?;
    if !handle.is_running() || handle.started_at_ms() != target.started_at_ms {
        return None;
    }
    handle.memory_bytes()
}

/// [`ProcessLookup`] поверх Windows.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProcessLookup;

impl ProcessLookup for SystemProcessLookup {
    fn by_pid(&self, process_id: u32) -> Option<TargetProcess> {
        let handle = ProcessHandle::open(process_id)?;
        if !handle.is_running() {
            return None;
        }
        handle.describe(process_id)
    }

    fn find_by_name(&self, executable: &str) -> Vec<TargetProcess> {
        let wanted = executable.trim();
        list_processes()
            .into_iter()
            .filter(|(_, name)| name.eq_ignore_ascii_case(wanted))
            .map(|(process_id, name)| {
                // Время старта нужно для идентичности. Если процесс не открывается, он всё равно
                // существует и должен считаться — иначе ручной режим молча не увидит игру,
                // запущенную с другими правами.
                let started_at_ms = ProcessHandle::open(process_id).and_then(|h| h.started_at_ms());
                TargetProcess::new(process_id, name, started_at_ms)
            })
            .collect()
    }

    fn is_same_run_alive(&self, target: &TargetProcess) -> bool {
        let Some(handle) = ProcessHandle::open(target.pid) else {
            return false;
        };
        // Тот же номер, но другое время старта — это уже другая игра (Windows переиспользует PID).
        handle.is_running() && handle.started_at_ms() == target.started_at_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own_process() -> TargetProcess {
        SystemProcessLookup
            .by_pid(std::process::id())
            .expect("собственный процесс открывается всегда")
    }

    #[test]
    fn the_current_process_is_found_with_a_name_and_a_start_time() {
        let me = own_process();
        assert_eq!(me.pid, std::process::id());
        assert!(me.executable.to_lowercase().ends_with(".exe"), "{}", me.executable);
        assert!(me.started_at_ms.is_some(), "свой процесс обязан сообщать время старта");
    }

    #[test]
    fn the_current_process_is_the_same_run_and_alive() {
        assert!(SystemProcessLookup.is_same_run_alive(&own_process()));
    }

    /// Тот же PID с чужим временем старта — другая игра.
    #[test]
    fn a_different_start_time_is_a_different_run() {
        let mut impostor = own_process();
        impostor.started_at_ms = impostor.started_at_ms.map(|at| at + 1);
        assert!(!SystemProcessLookup.is_same_run_alive(&impostor));
    }

    #[test]
    fn the_current_process_reports_its_memory() {
        let me = own_process();
        let bytes = process_memory(&me).expect("свою память процесс видит всегда");
        assert!(bytes > 0);
        let mut impostor = me;
        impostor.started_at_ms = impostor.started_at_ms.map(|at| at + 1);
        assert_eq!(process_memory(&impostor), None, "чужой запуск с тем же PID не считается");
    }

    #[test]
    fn a_process_that_does_not_exist_is_not_alive() {
        let ghost = TargetProcess::new(u32::MAX - 1, "ghost.exe", Some(1));
        assert!(!SystemProcessLookup.is_same_run_alive(&ghost));
        assert!(SystemProcessLookup.by_pid(u32::MAX - 1).is_none());
        assert!(SystemProcessLookup.by_pid(0).is_none(), "PID 0 — не процесс");
    }

    #[test]
    fn the_current_process_is_found_by_name() {
        let me = own_process();
        let found = SystemProcessLookup.find_by_name(&me.executable.to_uppercase());
        assert!(
            found.iter().any(|process| process.is_same_run_as(&me)),
            "поиск по имени без учёта регистра обязан найти себя"
        );
    }

    #[test]
    fn the_process_list_is_not_empty() {
        let processes = list_processes();
        assert!(processes.iter().any(|(pid, _)| *pid == std::process::id()));
    }

    /// Собственное окно никогда не выдвигается кандидатом.
    #[test]
    fn the_foreground_never_reports_our_own_process() {
        if let Some(candidate) = foreground_process(std::process::id()) {
            assert_ne!(candidate.pid, std::process::id());
            assert!(!is_shell_process(&candidate.executable));
        }
    }
}

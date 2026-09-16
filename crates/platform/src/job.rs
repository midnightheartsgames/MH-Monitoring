//! Job object, который убивает дочерние процессы вместе с приложением.
//!
//! Зачем: если приложение умирает не своей смертью — `TerminateProcess`, закрытие консоли,
//! снятие из диспетчера задач, — `Drop` не выполняется, и дочерний PresentMon остаётся жить.
//! Живой PresentMon держит ETW-сессию, а брошенная сессия ломает захват кадров на всей машине
//! (PLAN.md §2.1). Флаг `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` перекладывает эту заботу на ядро:
//! закрылся последний дескриптор задания — а он закрывается при любой смерти процесса, — и все
//! процессы в задании завершаются.
//!
//! Сессию это не гасит: её остановку по имени обеспечивают обработчик консоли и уборка сирот
//! при следующем старте. Job object закрывает другую дыру — живого ребёнка.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

use crate::sys::Win32Error;

pub struct KillOnCloseJob {
    handle: HANDLE,
}

// Дескриптор задания — непрозрачный номер ядра; пользоваться им из любого потока безопасно.
unsafe impl Send for KillOnCloseJob {}
unsafe impl Sync for KillOnCloseJob {}

impl KillOnCloseJob {
    pub fn new() -> Result<Self, Win32Error> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(Win32Error(unsafe { windows_sys::Win32::Foundation::GetLastError() }));
        }
        let job = Self { handle };

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(Win32Error(unsafe { windows_sys::Win32::Foundation::GetLastError() }));
        }
        Ok(job)
    }

    /// Помещает процесс в задание.
    ///
    /// По PID, а не по дескриптору: так вызывающему не нужно знать про Win32, а права ровно те,
    /// что нужны для этой операции.
    pub fn assign(&self, process_id: u32) -> Result<(), Win32Error> {
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, process_id) };
        if process.is_null() {
            return Err(Win32Error(unsafe { windows_sys::Win32::Foundation::GetLastError() }));
        }
        let ok = unsafe { AssignProcessToJobObject(self.handle, process) };
        let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        unsafe { CloseHandle(process) };
        if ok == 0 { Err(Win32Error(error)) } else { Ok(()) }
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // Закрытие последнего дескриптора завершает все процессы задания — ради этого оно и есть.
        unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// Главное свойство: закрыли задание — ребёнок умер, хотя его никто не убивал явно.
    #[test]
    fn closing_the_job_kills_the_child() {
        let job = KillOnCloseJob::new().expect("задание создаётся без прав администратора");

        // Долгоживущий ребёнок, который сам не завершится за время теста.
        let mut child = Command::new("cmd")
            .args(["/c", "ping", "-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("cmd запускается");
        job.assign(child.id()).expect("ребёнок помещается в задание");
        assert!(child.try_wait().unwrap().is_none(), "до закрытия задания ребёнок жив");

        drop(job);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        panic!("после закрытия задания ребёнок остался жив");
    }

    #[test]
    fn assigning_a_process_that_does_not_exist_fails_cleanly() {
        let job = KillOnCloseJob::new().expect("задание создаётся");
        assert!(job.assign(u32::MAX - 1).is_err());
    }
}

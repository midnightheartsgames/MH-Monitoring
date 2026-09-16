//! Перечисление и уборка ETW-сессий.
//!
//! Зачем это существует: realtime-сессия **переживает создавший её процесс**. Если её никто не
//! вычитывает, буферы переполняются и поток событий деградирует для всех потребителей на машине
//! — включая чужие инструменты. Это не теория: на этой машине сирота `MHMonitor-29244` от
//! убитого приложения обнуляла захват, пока её не остановили. После остановки, без единого
//! другого изменения, доставка событий выросла с нуля до шестнадцати тысяч за пятнадцать секунд
//! (`spikes/etw-frames/COVERAGE.md` §1).
//!
//! Симптом стоит выучить: **постоянные потери событий, не зависящие ни от нагрузки, ни от
//! настроек сессии**. Одинаковые потери при 10 и при 120 кадрах в секунду означают, что дело не
//! в вашем коде.
//!
//! Решение, какие имена трогать, принимает [`mh_core::session_name::should_sweep`] — здесь
//! только перечисление и остановка.

use mh_core::session_name::should_sweep;
use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, ControlTraceW, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_PROPERTIES,
    QueryAllTracesW,
};

use crate::sys::{PropsBuffer, Win32Error, wide};

/// Сколько сессий помещается в один запрос. На обычной машине их четыре десятка.
const MAX_SESSIONS: usize = 128;

/// Итог уборки.
#[derive(Debug, Default, Clone)]
pub struct Sweep {
    /// Остановленные сессии.
    pub stopped: Vec<String>,
    /// Те, которые остановить не удалось, с кодом ошибки.
    pub failed: Vec<(String, WIN32_ERROR)>,
    /// Сколько сессий всего в системе.
    pub total: u32,
    /// Ошибка перечисления. Без прав администратора список приходит пустым — это ожидаемо.
    pub query_error: Option<WIN32_ERROR>,
}

impl Sweep {
    /// Одна строка для лога.
    pub fn describe(&self) -> String {
        if let Some(code) = self.query_error {
            return format!("перечислить сессии не удалось: {}", Win32Error(code));
        }
        let mut text = format!(
            "сессий в системе {}, осиротевших остановлено {}",
            self.total,
            self.stopped.len()
        );
        for name in &self.stopped {
            text.push_str(&format!("; остановлена {name}"));
        }
        for (name, code) in &self.failed {
            text.push_str(&format!("; НЕ остановлена {name}: {}", Win32Error(*code)));
        }
        text
    }
}

/// Останавливает сессию по имени — то есть без хендла.
///
/// Именно так сессию гасим мы сами, а не надеемся на её владельца: на Windows
/// `TerminateProcess` не даёт процессу выполнить очистку, и сессия переживает его (PLAN.md §2.2).
/// Отсюда правило: гасить по известному нам имени, на каждом пути выхода.
pub fn stop_by_name(name: &str) -> WIN32_ERROR {
    let name_w = wide(name);
    let mut buffer = PropsBuffer::new();
    buffer.init(true);
    unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE { Value: 0 },
            name_w.as_ptr(),
            buffer.as_ptr(),
            EVENT_TRACE_CONTROL_STOP,
        )
    }
}

/// Перечисляет сессии и гасит осиротевшие, кроме `keep`.
///
/// `keep` — имя собственной сессии; пустая строка означает «своей ещё нет».
pub fn sweep_orphans(keep: &str) -> Sweep {
    let mut result = Sweep::default();

    let mut buffers: Vec<PropsBuffer> = (0..MAX_SESSIONS)
        .map(|_| {
            let mut buffer = PropsBuffer::new();
            buffer.init(true);
            buffer
        })
        .collect();
    let mut pointers: Vec<*mut EVENT_TRACE_PROPERTIES> =
        buffers.iter_mut().map(PropsBuffer::as_ptr).collect();

    let mut count = 0u32;
    let status = unsafe { QueryAllTracesW(pointers.as_mut_ptr(), MAX_SESSIONS as u32, &mut count) };

    // ERROR_MORE_DATA означает «сессий больше, чем влезло»: заполненные записи при этом валидны.
    // Отказываться от уборки из-за переполнения было бы ровно наоборот тому, что нужно.
    if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
        result.query_error = Some(status);
        return result;
    }
    result.total = count;

    for buffer in buffers.iter().take((count as usize).min(MAX_SESSIONS)) {
        let name = buffer.logger_name();
        if !should_sweep(&name, keep) {
            continue;
        }
        let status = stop_by_name(&name);
        if status == ERROR_SUCCESS {
            result.stopped.push(name);
        } else {
            result.failed.push((name, status));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Уборка на любой машине обязана оставаться безобидной: без прав она просто ничего не
    /// найдёт, а с правами — не тронет чужого. Чужое покрыто тестами `should_sweep` в `core`.
    #[test]
    fn a_sweep_is_safe_to_run_anywhere() {
        let sweep = sweep_orphans("MHMonitor-0");
        // Либо перечислили, либо честно сказали, что не смогли, — третьего быть не должно.
        assert!(sweep.query_error.is_some() || sweep.total >= sweep.stopped.len() as u32);
        assert!(!sweep.describe().is_empty());
    }

    /// Остановка несуществующей сессии — не паника и не успех, а внятный код ошибки.
    #[test]
    fn stopping_a_session_that_does_not_exist_returns_an_error_code() {
        let status = stop_by_name("MHMonitor-этой-сессии-нет-4294967295");
        assert_ne!(status, ERROR_SUCCESS);
    }
}

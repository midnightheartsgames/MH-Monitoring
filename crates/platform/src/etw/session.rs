//! Создание realtime ETW-сессии и её гарантированная остановка.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, WIN32_ERROR,
};
use windows_sys::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, ControlTraceW, EVENT_CONTROL_CODE_ENABLE_PROVIDER,
    EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2, StartTraceW,
    WNODE_FLAG_TRACED_GUID,
};

use crate::sys::{PropsBuffer, Win32Error, wide};

use super::hygiene::stop_by_name;

/// Описание провайдера, который надо включить в сессию.
///
/// GUID хранится как `u128`, а не как тип из `windows-sys`, намеренно: так провайдеров может
/// описывать крейт, который про Win32 ничего не знает, и `windows-sys` остаётся внутри
/// `platform`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    pub label: &'static str,
    pub guid: u128,
    pub level: u8,
    pub any_keyword: u64,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} {{{:08X}-{:04X}-{:04X}-{:016X}}} level {} keywords {:#x}",
            self.label,
            (self.guid >> 96) as u32,
            (self.guid >> 80) as u16,
            (self.guid >> 64) as u16,
            self.guid as u64,
            self.level,
            self.any_keyword
        )
    }
}

/// Счётчики сессии на момент остановки.
///
/// `events_lost` — первое, на что смотреть, когда кадров нет: ненулевое значение при любых
/// настройках означает сироту в системе, а не поломку в своём коде.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TraceStats {
    pub events_lost: u32,
    pub buffers_written: u32,
    pub real_time_buffers_lost: u32,
    pub log_buffers_lost: u32,
    pub number_of_buffers: u32,
}

#[derive(Debug)]
pub enum StartError {
    /// Realtime ETW требует администратора либо членства в группе `Performance Log Users`.
    /// Свой ETW-потребитель **не убирает** это требование (PLAN.md §2.3).
    AccessDenied,
    Other(WIN32_ERROR),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::AccessDenied => write!(
                formatter,
                "нет прав на realtime ETW: нужен администратор либо группа «Performance Log Users»"
            ),
            StartError::Other(code) => write!(formatter, "{}", Win32Error(*code)),
        }
    }
}

impl std::error::Error for StartError {}

/// Имя активной сессии для аварийных путей выхода.
static ACTIVE_SESSION: OnceLock<String> = OnceLock::new();
/// Гарантия, что остановка выполняется ровно один раз, кто бы до неё ни дошёл первым.
static STOP_DONE: AtomicBool = AtomicBool::new(false);

/// Ставит panic hook, который гасит сессию.
///
/// `Drop` покрывает обычный выход и раскрутку стека, но паника внутри `extern "system"`
/// callback'а стек не разворачивает — она завершает процесс. Hook срабатывает раньше.
pub fn install_panic_cleanup() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        stop_active_once();
        previous(info);
    }));
}

/// Останавливает активную сессию, если это ещё не сделано.
pub fn stop_active_once() -> bool {
    if STOP_DONE.swap(true, Ordering::SeqCst) {
        return false;
    }
    if let Some(name) = ACTIVE_SESSION.get() {
        stop_by_name(name);
    }
    true
}

/// Живая realtime-сессия.
pub struct Session {
    handle: CONTROLTRACE_HANDLE,
    name: String,
    name_w: Vec<u16>,
}

impl Session {
    /// Создаёт сессию. Имя — одно на запуск процесса (PLAN.md §2.1.1).
    pub fn start(name: &str) -> Result<Self, StartError> {
        // Имя попадает в статик до первого StartTrace: если он упадёт на полпути и всё же
        // оставит сессию, аварийные пути будут знать, что гасить.
        let _ = ACTIVE_SESSION.set(name.to_string());
        let name_w = wide(name);

        match Self::try_start(name, &name_w) {
            Ok(session) => Ok(session),
            Err(ERROR_ALREADY_EXISTS) => {
                // Имя наше — значит это сирота от прошлого запуска. Для чужого имени так
                // делать нельзя (PLAN.md §2.1.2).
                stop_by_name(name);
                Self::try_start(name, &name_w).map_err(Self::classify)
            }
            Err(other) => Err(Self::classify(other)),
        }
    }

    fn classify(code: WIN32_ERROR) -> StartError {
        if code == ERROR_ACCESS_DENIED { StartError::AccessDenied } else { StartError::Other(code) }
    }

    fn try_start(name: &str, name_w: &[u16]) -> Result<Self, WIN32_ERROR> {
        let mut buffer = PropsBuffer::new();
        // realtime без файла: LogFileNameOffset обязан быть нулевым.
        buffer.init(false);
        buffer.write_logger_name(name_w);

        {
            let props = unsafe { &mut *buffer.as_ptr() };
            props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            // ClientContext = 1 — таймстемпы событий в единицах QPC. Единственный режим, в
            // котором дельта между Present'ами считается без пересчёта разрешений таймера.
            props.Wnode.ClientContext = 1;
            props.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            // Размер и число буферов оставлены на усмотрение ETW намеренно. В P0 проверены
            // четыре конфигурации (умолчания, 8, 64 и 128 КБ с разным числом буферов) — на
            // доставку не влияет ни одна. Настраивать имеет смысл только с измерением в руках.
        }

        let mut handle = CONTROLTRACE_HANDLE { Value: 0 };
        let status = unsafe { StartTraceW(&mut handle, name_w.as_ptr(), buffer.as_ptr()) };
        if status != ERROR_SUCCESS {
            return Err(status);
        }
        STOP_DONE.store(false, Ordering::SeqCst);
        Ok(Session { handle, name: name.to_string(), name_w: name_w.to_vec() })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Включает провайдера.
    ///
    /// Вызывать **до** открытия потребителя: при включении после запуска `ProcessTrace`
    /// потребителю не доставалось ни одного события. Проверено экспериментально в P0.
    ///
    /// `level_override` полезен потому, что у Present-событий DXGI в манифесте `level = 0`
    /// (LogAlways): они проходят при любом уровне, и 255 лишь тянет в сессию вербозный поток
    /// со всей системы.
    pub fn enable_provider(
        &self,
        provider: &Provider,
        level_override: Option<u8>,
    ) -> Result<(), WIN32_ERROR> {
        let guid = windows_sys::core::GUID::from_u128(provider.guid);
        let status = unsafe {
            EnableTraceEx2(
                self.handle,
                &guid,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                level_override.unwrap_or(provider.level),
                provider.any_keyword,
                0,
                0,
                std::ptr::null(),
            )
        };
        if status == ERROR_SUCCESS { Ok(()) } else { Err(status) }
    }

    /// Останавливает сессию и возвращает её финальные счётчики.
    ///
    /// Остановка — единственное, что разблокирует `ProcessTrace` в потоке потребителя
    /// (PLAN.md §2.8).
    pub fn stop(&mut self) -> Option<TraceStats> {
        if STOP_DONE.swap(true, Ordering::SeqCst) {
            return None;
        }
        let mut buffer = PropsBuffer::new();
        buffer.init(true);
        let status = unsafe {
            ControlTraceW(
                self.handle,
                self.name_w.as_ptr(),
                buffer.as_ptr(),
                EVENT_TRACE_CONTROL_STOP,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let props = unsafe { &*buffer.as_ptr() };
        Some(TraceStats {
            events_lost: props.EventsLost,
            buffers_written: props.BuffersWritten,
            real_time_buffers_lost: props.RealTimeBuffersLost,
            log_buffers_lost: props.LogBuffersLost,
            number_of_buffers: props.NumberOfBuffers,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Последний рубеж: обычный выход, ранний возврат по ошибке, раскрутка стека при панике.
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Без прав сессия не поднимется, и это должно быть сказано понятным текстом, а не кодом.
    /// Тест не требует прав: он проверяет обе ветки классификации.
    #[test]
    fn access_denied_is_reported_in_words() {
        let denied = StartError::AccessDenied.to_string();
        assert!(denied.contains("Performance Log Users"), "подсказка про группу обязана быть");

        let other = StartError::Other(2).to_string();
        assert!(!other.is_empty());
    }

    /// Запуск от обычного пользователя обязан честно провалиться, а не создать что-то
    /// наполовину. С правами — подняться и остановиться без следа.
    #[test]
    fn starting_a_session_either_works_or_fails_cleanly() {
        let name = mh_core::session_name(std::process::id());
        match Session::start(&name) {
            Ok(mut session) => {
                assert_eq!(session.name(), name);
                assert!(session.stop().is_some(), "своя сессия обязана остановиться");
            }
            Err(StartError::AccessDenied) => {}
            Err(StartError::Other(code)) => panic!("неожиданная ошибка: {}", Win32Error(code)),
        }
    }
}

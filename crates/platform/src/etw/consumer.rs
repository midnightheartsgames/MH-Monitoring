//! Потребитель realtime-сессии: `OpenTrace` + `ProcessTrace` + callback.

use std::ffi::c_void;

use std::sync::Arc;
use windows_sys::Win32::Foundation::{GetLastError, WIN32_ERROR};

use windows_sys::Win32::System::Diagnostics::Etw::{
    CloseTrace, EVENT_RECORD, EVENT_TRACE_LOGFILEW, OpenTraceW, PEVENT_RECORD_CALLBACK,
    PEVENT_TRACE_BUFFER_CALLBACKW, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME,
    PROCESSTRACE_HANDLE, ProcessTrace,
};

use crate::sys::wide;

const INVALID_PROCESSTRACE_HANDLE: u64 = u64::MAX;

/// Открытый на чтение трейс.
///
/// `ProcessTrace` блокирует вызвавший поток до остановки сессии, поэтому его крутит отдельный
/// поток, а управление остаётся у основного (PLAN.md §2.8).
pub struct Consumer {
    handle: PROCESSTRACE_HANDLE,
    /// **Структуру нельзя оставлять на стеке.**
    ///
    /// ETW сохраняет указатель на неё, а не копирует целиком. Пока она была локальной
    /// переменной, `OpenTrace` возвращал успех, `ProcessTrace` честно блокировал поток до
    /// остановки сессии и возвращал `ERROR_SUCCESS` — и при этом не приходило **ни одного**
    /// буфера. Диагноз стоил половины отладки P0.
    _logfile: Box<EVENT_TRACE_LOGFILEW>,
    /// `OpenTraceW` получает указатель на это имя; держим его живым всё время работы трейса.
    _name_w: Vec<u16>,
    /// Приёмник событий, если трейс открыт через [`Consumer::open_with_sink`]. Живёт ровно
    /// столько же, сколько сам трейс, — на него смотрит callback.
    sink: Option<Box<Arc<dyn EventSink>>>,
}

// PROCESSTRACE_HANDLE — обычное 64-битное число. Данные, на которые ETW держит указатели,
// живут в куче и от переезда владельца не меняются.
unsafe impl Send for Consumer {}

impl Consumer {
    /// Открывает трейс по имени уже созданной сессии.
    ///
    /// `context` придёт в callback как `EVENT_RECORD.UserContext`. Вызывающий отвечает за то,
    /// чтобы указуемое жило дольше потока, который крутит [`Self::process`].
    ///
    /// # Safety
    ///
    /// `context` должен оставаться валидным до завершения `process`, а `event_callback` —
    /// не паниковать: паника в `extern "system"` не разворачивает стек, а убивает процесс,
    /// вместе с шансом остановить ETW-сессию.
    pub unsafe fn open(
        session_name: &str,
        event_callback: PEVENT_RECORD_CALLBACK,
        buffer_callback: PEVENT_TRACE_BUFFER_CALLBACKW,
        context: *mut c_void,
    ) -> Result<Self, WIN32_ERROR> {
        let mut name_w = wide(session_name);
        let mut logfile: Box<EVENT_TRACE_LOGFILEW> = Box::new(unsafe { std::mem::zeroed() });
        logfile.LoggerName = name_w.as_mut_ptr();
        logfile.LogFileName = std::ptr::null_mut();
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.Anonymous2.EventRecordCallback = event_callback;
        logfile.BufferCallback = buffer_callback;
        logfile.Context = context;

        let handle = unsafe { OpenTraceW(&mut *logfile) };
        if handle.Value == INVALID_PROCESSTRACE_HANDLE {
            return Err(unsafe { GetLastError() });
        }
        Ok(Consumer { handle, _logfile: logfile, _name_w: name_w, sink: None })
    }

    /// Блокирует поток до остановки сессии.
    ///
    /// **Судить о том, подключился ли потребитель, надо отсюда, а не по результату
    /// [`Self::open`].** `OpenTraceW` отдаёт валидный хендл даже для несуществующей сессии —
    /// проверено тестом ниже; ошибка всплывает только здесь.
    ///
    /// Момент возврата информативнее самого статуса: мгновенный возврат с `ERROR_SUCCESS`
    /// означает, что потребитель не подключился, и все события уйдут в `EventsLost`.
    pub fn process(&self) -> WIN32_ERROR {
        unsafe { ProcessTrace(&self.handle, 1, std::ptr::null(), std::ptr::null()) }
    }

    /// Счётчики, которые ETW обновляет прямо в структуре потребителя.
    pub fn counters(&self) -> (u32, u32) {
        (self._logfile.BuffersRead, self._logfile.EventsLost)
    }
}

/// Чем закончился `ProcessTrace` и что потребитель успел увидеть.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessStatus {
    pub status: WIN32_ERROR,
    pub buffers_read: u32,
    pub events_lost: u32,
}

/// Одно событие в безопасном виде.
///
/// `EventHeader` уже содержит PID и таймстемп, поэтому для frametime разбирать payload не
/// обязательно — но он всё равно отдаётся: без флагов не отличить настоящий кадр от тестового
/// вызова `Present` (`sources::frames::etw_dxgi::payload`).
#[derive(Debug, Clone, Copy)]
pub struct EventInfo<'a> {
    /// GUID провайдера числом — сравнивать его может крейт, не знающий про Win32.
    pub provider_guid: u128,
    pub process_id: u32,
    pub thread_id: u32,
    /// В единицах QPC, потому что сессия создана с `Wnode.ClientContext = 1`.
    pub timestamp_qpc: i64,
    pub event_id: u16,
    pub opcode: u8,
    pub version: u8,
    pub user_data: &'a [u8],
}

/// Куда потребитель отдаёт события.
///
/// Реализация обязана быть **быстрой и не паниковать**: вызывается она из callback'а ETW, а
/// паника в `extern "system"` не разворачивает стек — она убивает процесс вместе с шансом
/// остановить сессию. Поэтому `platform` ловит панику сам (см. `dispatch_event`), но
/// рассчитывать на это не стоит.
pub trait EventSink: Send + Sync {
    fn on_event(&self, event: &EventInfo<'_>);

    /// Вызывается на каждый доставленный буфер. `false` останавливает обработку.
    ///
    /// `events_lost` — тот самый счётчик, ненулевое значение которого при любых настройках
    /// означает осиротевшую сессию в системе, а не поломку в своём коде.
    fn on_buffer(&self, _buffers_read: u32, _events_lost: u32) -> bool {
        true
    }
}

fn guid_to_u128(guid: &windows_sys::core::GUID) -> u128 {
    ((guid.data1 as u128) << 96)
        | ((guid.data2 as u128) << 80)
        | ((guid.data3 as u128) << 64)
        | u64::from_be_bytes(guid.data4) as u128
}

unsafe extern "system" fn dispatch_event(record: *mut EVENT_RECORD) {
    if record.is_null() {
        return;
    }
    let record = unsafe { &*record };
    let sink = record.UserContext as *const Arc<dyn EventSink>;
    if sink.is_null() {
        return;
    }
    let header = &record.EventHeader;
    let user_data = if record.UserData.is_null() {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(record.UserData.cast::<u8>(), record.UserDataLength as usize)
        }
    };
    let info = EventInfo {
        provider_guid: guid_to_u128(&header.ProviderId),
        process_id: header.ProcessId,
        thread_id: header.ThreadId,
        timestamp_qpc: header.TimeStamp,
        event_id: header.EventDescriptor.Id,
        opcode: header.EventDescriptor.Opcode,
        version: header.EventDescriptor.Version,
        user_data,
    };
    // Паника отсюда завершила бы процесс, не дав остановить ETW-сессию. Ловим на всякий
    // случай, хотя реализация обязана не паниковать.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (*sink).on_event(&info)
    }));
}

unsafe extern "system" fn dispatch_buffer(logfile: *mut EVENT_TRACE_LOGFILEW) -> u32 {
    if logfile.is_null() {
        return 1;
    }
    let logfile = unsafe { &*logfile };
    let sink = logfile.Context as *const Arc<dyn EventSink>;
    if sink.is_null() {
        return 1;
    }
    let keep_going = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (*sink).on_buffer(logfile.BuffersRead, logfile.EventsLost)
    }))
    .unwrap_or(true);
    u32::from(keep_going)
}

impl Consumer {
    /// Открывает трейс, отдавая события в [`EventSink`].
    ///
    /// Безопасная обёртка над [`Self::open`]: контекст и время его жизни берёт на себя сам
    /// потребитель, а вызывающий получает разобранные события вместо сырых записей ETW.
    pub fn open_with_sink(
        session_name: &str,
        sink: Arc<dyn EventSink>,
    ) -> Result<Self, WIN32_ERROR> {
        // Двойная упаковка не лишняя: `Arc<dyn …>` — толстый указатель, а в `Context` помещается
        // только тонкий. Box даёт адрес, по которому толстый указатель лежит целиком.
        let boxed: Box<Arc<dyn EventSink>> = Box::new(sink);
        let context = Box::into_raw(boxed);
        let opened = unsafe {
            Self::open(session_name, Some(dispatch_event), Some(dispatch_buffer), context.cast())
        };
        match opened {
            Ok(mut consumer) => {
                consumer.sink = Some(unsafe { Box::from_raw(context) });
                Ok(consumer)
            }
            Err(code) => {
                // Открыть не удалось — забираем владение обратно, иначе это утечка.
                drop(unsafe { Box::from_raw(context) });
                Err(code)
            }
        }
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        // `CloseTrace` корректен только после возврата из `ProcessTrace`, иначе вернёт
        // ERROR_CTX_CLOSE_PENDING. Порядок обеспечивает вызывающий, присоединяя поток.
        unsafe { CloseTrace(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Зафиксировано измерением: `OpenTraceW` на несуществующей сессии **возвращает успех**.
    /// Ошибка приходит только из `ProcessTrace`.
    ///
    /// Из этого следует правило для вызывающего: успешное открытие не значит, что потребитель
    /// подключён. Единственный надёжный признак — статус и момент возврата `ProcessTrace`.
    #[test]
    fn a_missing_session_is_only_detected_by_process_not_by_open() {
        let consumer = unsafe {
            Consumer::open("MHMonitor-нет-такой-сессии", None, None, std::ptr::null_mut())
        };
        let Ok(consumer) = consumer else {
            // Если однажды поведение изменится и открытие начнёт честно падать — это тоже
            // приемлемо, и тест не должен этому мешать.
            return;
        };

        // `process` на несуществующей сессии обязан вернуться сразу. Крутим его в отдельном
        // потоке с запасом по времени: зависание здесь не должно вешать весь набор тестов.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let status = consumer.process();
            let _ = sender.send(status);
        });

        match receiver.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(status) => assert_ne!(status, 0, "ProcessTrace обязан сообщить, что сессии нет"),
            Err(_) => panic!("ProcessTrace завис на несуществующей сессии"),
        }
    }
}

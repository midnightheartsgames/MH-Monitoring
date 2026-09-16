//! ETW: создание realtime-сессии, её гарантированная остановка, уборка сирот и потребитель.
//!
//! Главное требование модуля — сессия не должна пережить процесс ни на одном пути выхода.
//! Брошенная realtime-сессия ломает захват кадров на всей машине, включая чужие инструменты
//! (PLAN.md §2.1). Поэтому остановка продублирована трижды: `Drop` у [`Session`], обработчик
//! Ctrl+C и panic hook — все три идут через один и тот же флаг, так что срабатывает ровно одна.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR,
};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, ControlTraceW, EVENT_CONTROL_CODE_ENABLE_PROVIDER,
    EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES,
    EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2, OpenTraceW, PROCESS_TRACE_MODE_EVENT_RECORD,
    PROCESS_TRACE_MODE_REAL_TIME, PROCESSTRACE_HANDLE, PEVENT_RECORD_CALLBACK,
    PEVENT_TRACE_BUFFER_CALLBACKW, ProcessTrace, QueryAllTracesW, WNODE_FLAG_TRACED_GUID,
};

use crate::sys::{PROPS_BUFFER_BYTES, PropsBuffer, Win32Error, last_error, wide};

/// Префикс имён сессий спайка.
pub const SESSION_PREFIX: &str = "MHMonitorSpike-";

/// Префикс, по которому идёт уборка сирот. Шире собственного: сюда попадают и сессии
/// рабочего приложения `MHMonitor-<pid>`, и сессии спайка `MHMonitorSpike-<pid>`.
///
/// Так и требует PLAN.md §2.1.4, и это не перестраховка. Замеренный случай: в системе
/// работала сессия `MHMonitor-29244`, оставшаяся от убитого старого приложения. Её никто не
/// вычитывал, и захват событий деградировал для ВСЕХ потребителей на машине — наш спайк
/// получал единицы событий при тысячах потерянных, независимо от уровня провайдера,
/// размера буферов и частоты кадров у цели.
///
/// Дефиса в конце нет намеренно: собственное имя спайка `MHMonitorSpike-<pid>` не начинается
/// с `MHMonitor-`, и с дефисом уборка проходила бы мимо своих же сирот.
///
/// Никогда не расширять этот префикс до пустой строки: имя `PresentMon` общее для чужих
/// инструментов, и останавливать его — значит ломать чужой захват (PLAN.md §2.1.2).
pub const SWEEP_PREFIX: &str = "MHMonitor";

/// Провайдеры сняты с дампа живой сессии PresentMon (PLAN.md §2.9), а не из документации.
/// Keyword `0x8000000000000002` — ровно то значение, что стоит в реальной сессии: старший бит
/// относится к зарезервированным Microsoft keywords, `0x2` — к событиям Present.
pub struct Provider {
    pub label: &'static str,
    pub guid: windows_sys::core::GUID,
    pub level: u8,
    pub any_keyword: u64,
}

pub const PROVIDER_DXGI: Provider = Provider {
    label: "DXGI",
    guid: windows_sys::core::GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9),
    level: 255,
    any_keyword: 0x8000_0000_0000_0002,
};

pub const PROVIDER_D3D9: Provider = Provider {
    label: "D3D9",
    guid: windows_sys::core::GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9),
    level: 255,
    any_keyword: 0x8000_0000_0000_0002,
};

// --- аварийная остановка -------------------------------------------------------------------

/// Имя активной сессии в виде UTF-16. Нужно обработчику Ctrl+C и panic hook: у них нет доступа
/// к [`Session`], а остановить сессию они обязаны.
static ACTIVE_SESSION: OnceLock<Vec<u16>> = OnceLock::new();
/// Гарантия, что остановка выполняется ровно один раз, кто бы до неё ни дошёл первым.
static STOP_DONE: AtomicBool = AtomicBool::new(false);
/// Сигнал основному циклу: пора закругляться.
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn stop_requested() -> bool {
    STOP_REQUESTED.load(Ordering::SeqCst)
}

pub fn request_stop() {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

/// Останавливает сессию по имени — то есть без хендла.
///
/// Именно так сессию гасим мы сами, а не надеемся на владельца: `TerminateProcess` не даёт
/// процессу выполнить очистку, и брошенная сессия переживает его (PLAN.md §2.2).
pub fn stop_session_by_name(name: &[u16]) -> WIN32_ERROR {
    let mut buf = PropsBuffer::new();
    buf.init(true);
    unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE { Value: 0 },
            name.as_ptr(),
            buf.as_ptr(),
            EVENT_TRACE_CONTROL_STOP,
        )
    }
}

/// Останавливает активную сессию, если это ещё не сделано. Возвращает `true`, если остановка
/// выполнена именно этим вызовом.
fn stop_active_once() -> bool {
    if STOP_DONE.swap(true, Ordering::SeqCst) {
        return false;
    }
    if let Some(name) = ACTIVE_SESSION.get() {
        stop_session_by_name(name);
    }
    true
}

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> windows_sys::core::BOOL {
    // CTRL_CLOSE_EVENT завершает процесс сразу после возврата из обработчика, поэтому
    // останавливаем сессию прямо здесь, а не через флаг: до Drop дело может не дойти.
    stop_active_once();
    STOP_REQUESTED.store(true, Ordering::SeqCst);
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => 1,
        _ => 0,
    }
}

/// Ставит обработчик Ctrl+C и panic hook. Вызывать до `StartTrace`.
pub fn install_emergency_cleanup() {
    unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) };

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if stop_active_once() {
            eprintln!("[cleanup] паника: ETW-сессия остановлена");
        }
        previous(info);
    }));
}

// --- уборка сирот --------------------------------------------------------------------------

pub struct OrphanSweep {
    pub stopped: Vec<String>,
    pub failed: Vec<(String, WIN32_ERROR)>,
    pub query_error: Option<WIN32_ERROR>,
    pub total_sessions: u32,
}

/// Перечисляет realtime-сессии и гасит все с нашим префиксом, кроме `keep`.
///
/// Без этого шага сирота после падения или `TerminateProcess` живёт до перезагрузки и делает
/// следующий замер пустым — самая вероятная причина «спайк не даёт кадров» (PLAN.md §2.1.4).
pub fn sweep_orphans(keep: &str) -> OrphanSweep {
    const MAX_SESSIONS: usize = 128;

    let mut result =
        OrphanSweep { stopped: Vec::new(), failed: Vec::new(), query_error: None, total_sessions: 0 };

    let mut buffers: Vec<PropsBuffer> = (0..MAX_SESSIONS)
        .map(|_| {
            let mut b = PropsBuffer::new();
            b.init(true);
            b
        })
        .collect();
    let mut pointers: Vec<*mut EVENT_TRACE_PROPERTIES> =
        buffers.iter_mut().map(|b| b.as_ptr()).collect();

    let mut count = 0u32;
    let status = unsafe {
        QueryAllTracesW(pointers.as_mut_ptr(), MAX_SESSIONS as u32, &mut count)
    };
    // ERROR_MORE_DATA значит «сессий больше, чем влезло»: заполненные записи при этом валидны,
    // и отказываться от уборки из-за переполнения было бы ровно наоборот тому, что нужно.
    if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
        result.query_error = Some(status);
        return result;
    }
    result.total_sessions = count;
    let count = count.min(MAX_SESSIONS as u32);

    for buf in buffers.iter().take(count as usize) {
        let name = buf.logger_name();
        if !name.starts_with(SWEEP_PREFIX) || name == keep {
            continue;
        }
        let status = stop_session_by_name(&wide(&name));
        if status == ERROR_SUCCESS {
            result.stopped.push(name);
        } else {
            result.failed.push((name, status));
        }
    }
    result
}

// --- сессия --------------------------------------------------------------------------------

/// Параметры буферов сессии. Ноль означает «оставить на усмотрение ETW».
#[derive(Debug, Default, Clone, Copy)]
pub struct BufferConfig {
    pub buffer_kb: u32,
    pub min_buffers: u32,
    pub max_buffers: u32,
    pub flush_seconds: u32,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TraceStats {
    pub events_lost: u32,
    pub buffers_written: u32,
    pub real_time_buffers_lost: u32,
    pub log_buffers_lost: u32,
    pub number_of_buffers: u32,
}

pub enum StartError {
    AccessDenied,
    Other(WIN32_ERROR),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::AccessDenied => write!(
                f,
                "нет прав на realtime ETW. Запустите от администратора либо добавьте \
                 пользователя в группу «Performance Log Users» (PLAN.md §2.3)"
            ),
            StartError::Other(code) => write!(f, "{}", Win32Error(*code)),
        }
    }
}

pub struct Session {
    handle: CONTROLTRACE_HANDLE,
    name: String,
    name_w: Vec<u16>,
}

impl Session {
    /// Создаёт realtime-сессию. Имя — одно на запуск процесса (PLAN.md §2.1.1).
    pub fn start(name: &str, buffers: BufferConfig) -> Result<Self, StartError> {
        let name_w = wide(name);
        // Имя кладём в статик до первого StartTrace: если он упадёт на полпути и оставит
        // сессию, аварийные пути всё равно будут знать, что гасить.
        let _ = ACTIVE_SESSION.set(name_w.clone());

        match Self::try_start(name, &name_w, buffers) {
            Ok(session) => Ok(session),
            Err(ERROR_ALREADY_EXISTS) => {
                // Имя наше, по префиксу. Значит это сирота от прошлого запуска — гасим и
                // пробуем ещё раз. Для чужого имени так делать нельзя (PLAN.md §2.1).
                eprintln!("[cleanup] сессия «{name}» уже существует — останавливаю и повторяю");
                stop_session_by_name(&name_w);
                Self::try_start(name, &name_w, buffers).map_err(|code| match code {
                    ERROR_ACCESS_DENIED => StartError::AccessDenied,
                    other => StartError::Other(other),
                })
            }
            Err(ERROR_ACCESS_DENIED) => Err(StartError::AccessDenied),
            Err(other) => Err(StartError::Other(other)),
        }
    }

    fn try_start(
        name: &str,
        name_w: &[u16],
        buffers: BufferConfig,
    ) -> Result<Self, WIN32_ERROR> {
        let mut buf = PropsBuffer::new();
        buf.init(false); // realtime без файла: LogFileNameOffset обязан быть нулевым
        buf.write_logger_name(name_w);

        {
            let props = unsafe { &mut *buf.as_ptr() };
            props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            // ClientContext = 1 → таймстемпы событий в единицах QPC. Это единственный режим,
            // в котором дельта между Present'ами считается без пересчёта разрешений таймера.
            props.Wnode.ClientContext = 1;
            props.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            // Ноль в любом поле означает «не задавать»: ETW подставит собственное значение.
            // Значения по умолчанию у него разумные, а неудачно выбранный размер буфера
            // ломает доставку в реальном времени, поэтому переопределять их стоит только
            // осознанно — см. `--buffer-kb` и соседние ключи.
            props.BufferSize = buffers.buffer_kb;
            props.MinimumBuffers = buffers.min_buffers;
            props.MaximumBuffers = buffers.max_buffers;
            props.FlushTimer = buffers.flush_seconds;
        }

        let mut handle = CONTROLTRACE_HANDLE { Value: 0 };
        let status = unsafe { StartTraceW(&mut handle, name_w.as_ptr(), buf.as_ptr()) };
        if status != ERROR_SUCCESS {
            return Err(status);
        }
        STOP_DONE.store(false, Ordering::SeqCst);
        Ok(Session { handle, name: name.to_string(), name_w: name_w.to_vec() })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// `level_override` задаёт уровень вместо значения из описания провайдера.
    ///
    /// В манифесте DXGI у Present-событий `level = 0` (LogAlways) — они проходят при любом
    /// уровне. Значит 255 ничего не добавляет к нужным событиям, а только тянет в сессию
    /// вербозный поток со всей системы.
    pub fn enable_provider(
        &self,
        provider: &Provider,
        level_override: Option<u8>,
    ) -> Result<(), WIN32_ERROR> {
        let status = unsafe {
            EnableTraceEx2(
                self.handle,
                &provider.guid,
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

    /// Останавливает сессию и возвращает её финальные счётчики: `ControlTrace(STOP)` заполняет
    /// переданный буфер актуальной статистикой. `EventsLost > 0` — прямой признак того, что
    /// происходит в §2.1, поэтому эти числа всегда попадают в отчёт.
    pub fn stop(&mut self) -> Option<TraceStats> {
        if STOP_DONE.swap(true, Ordering::SeqCst) {
            return None;
        }
        let mut buf = PropsBuffer::new();
        buf.init(true);
        let status = unsafe {
            ControlTraceW(
                self.handle,
                self.name_w.as_ptr(),
                buf.as_ptr(),
                EVENT_TRACE_CONTROL_STOP,
            )
        };
        if status != ERROR_SUCCESS {
            eprintln!("[cleanup] ControlTrace(STOP) вернул {}", Win32Error(status));
            return None;
        }
        let props = unsafe { &*buf.as_ptr() };
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
        // Последний рубеж: обычный выход, ранний `return` по ошибке, раскрутка стека при панике.
        self.stop();
    }
}

// --- потребитель ---------------------------------------------------------------------------

/// Открытый на чтение трейс. `ProcessTrace` блокирует поток до остановки сессии, поэтому
/// его крутит отдельный поток, а управление остаётся у основного (PLAN.md §2.8).
pub struct Consumer {
    handle: PROCESSTRACE_HANDLE,
    /// `OpenTraceW` получает указатель на эту структуру и на имя внутри неё. Держим обе
    /// живыми всё время работы трейса: что именно ETW копирует, а на что оставляет указатель,
    /// документация не оговаривает, и стековый временный объект здесь — ставка без нужды.
    logfile: Box<EVENT_TRACE_LOGFILEW>,
    _name_w: Vec<u16>,
}

// PROCESSTRACE_HANDLE — обычное 64-битное число, его безопасно переносить в другой поток.
// Указатель на имя живёт в куче и от переезда владельца не меняется.
unsafe impl Send for Consumer {}

const INVALID_PROCESSTRACE_HANDLE: u64 = u64::MAX;

impl Consumer {
    pub fn open(
        session_name: &str,
        event_callback: PEVENT_RECORD_CALLBACK,
        buffer_callback: PEVENT_TRACE_BUFFER_CALLBACKW,
        context: *mut std::ffi::c_void,
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
            return Err(last_error());
        }
        eprintln!("[consumer] OpenTrace ok, handle 0x{:X}", handle.Value);
        Ok(Consumer { handle, logfile, _name_w: name_w })
    }

    /// Блокирует поток до остановки сессии.
    pub fn process(&self) -> WIN32_ERROR {
        unsafe { ProcessTrace(&self.handle, 1, std::ptr::null(), std::ptr::null()) }
    }

    /// Счётчики, которые ETW обновляет прямо в структуре потребителя.
    pub fn counters(&self) -> (u32, u32) {
        (self.logfile.BuffersRead, self.logfile.EventsLost)
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        // CloseTrace корректен только после возврата из ProcessTrace, иначе вернёт
        // ERROR_CTX_CLOSE_PENDING. Порядок гарантируется join'ом потока в main.
        unsafe { CloseTrace(self.handle) };
    }
}

// Импорты, которые нужны только внутри реализации, — держим их рядом с использованием,
// чтобы шапка модуля не превращалась в простыню.
use windows_sys::Win32::System::Diagnostics::Etw::{CloseTrace, StartTraceW};

const _: () = assert!(PROPS_BUFFER_BYTES > size_of::<EVENT_TRACE_PROPERTIES>());

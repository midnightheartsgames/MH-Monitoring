//! Мелкие обёртки над Win32, которыми пользуются остальные модули спайка.

use std::fmt;

use windows_sys::Win32::Foundation::{GetLastError, WIN32_ERROR};
use windows_sys::Win32::System::Diagnostics::Debug::{
    FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
};
use windows_sys::Win32::System::Diagnostics::Etw::EVENT_TRACE_PROPERTIES;
use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;

/// UTF-16 строка с завершающим нулём — в таком виде Win32 принимает `PCWSTR`/`PWSTR`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Читает UTF-16 строку до первого нуля.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn last_error() -> WIN32_ERROR {
    unsafe { GetLastError() }
}

/// Системный текст ошибки. Нужен потому, что голый код вроде `5` в логе спайка бесполезен,
/// а различать «нет прав» и «сессия уже есть» приходится постоянно (PLAN.md §2.3, §2.6).
pub struct Win32Error(pub WIN32_ERROR);

impl fmt::Display for Win32Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0u16; 512];
        let len = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                std::ptr::null(),
                self.0,
                0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                std::ptr::null(),
            )
        };
        let text = from_wide(&buf[..len as usize]);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            write!(f, "код {}", self.0)
        } else {
            write!(f, "{text} (код {})", self.0)
        }
    }
}

pub fn qpc_frequency() -> i64 {
    // Сессия создаётся с `Wnode.ClientContext = 1`, то есть таймстемпы событий — сырые
    // значения QPC. Значит и частота берётся та же, что у QueryPerformanceCounter.
    let mut freq = 0i64;
    let ok = unsafe { QueryPerformanceFrequency(&mut freq) };
    if ok == 0 || freq == 0 { 10_000_000 } else { freq }
}

pub const PROPS_SIZE: usize = size_of::<EVENT_TRACE_PROPERTIES>();
/// Сколько байт резервируется под каждое из двух имён, лежащих за структурой.
pub const NAME_BYTES: usize = 1024;
pub const PROPS_BUFFER_BYTES: usize = PROPS_SIZE + 2 * NAME_BYTES;

/// Буфер под `EVENT_TRACE_PROPERTIES`.
///
/// Две причины, по которым это не `Vec<u8>`:
/// * структура требует выравнивания на 8 байт, а `Vec<u8>` его не гарантирует;
/// * сразу за структурой в той же памяти лежат имя сессии и имя файла, на которые указывают
///   `LoggerNameOffset` / `LogFileNameOffset`, поэтому выделять нужно больше `size_of`.
pub struct PropsBuffer {
    words: Vec<u64>,
}

impl PropsBuffer {
    pub fn new() -> Self {
        Self { words: vec![0u64; PROPS_BUFFER_BYTES.div_ceil(8)] }
    }

    pub fn byte_len(&self) -> usize {
        self.words.len() * 8
    }

    pub fn as_ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
        self.words.as_mut_ptr().cast()
    }

    /// Заполняет поля, обязательные для любого вызова Start/Control/Query.
    ///
    /// `with_log_file_name`: для realtime-сессии без файла `StartTrace` хочет `LogFileNameOffset = 0`,
    /// а `ControlTrace`/`QueryAllTraces` — валидное смещение, куда они впишут имя.
    pub fn init(&mut self, with_log_file_name: bool) {
        let total = self.byte_len() as u32;
        let props = unsafe { &mut *self.as_ptr() };
        props.Wnode.BufferSize = total;
        props.LoggerNameOffset = PROPS_SIZE as u32;
        props.LogFileNameOffset =
            if with_log_file_name { (PROPS_SIZE + NAME_BYTES) as u32 } else { 0 };
    }

    /// Кладёт имя сессии в область за структурой.
    ///
    /// `StartTraceW` копирует имя туда сам, но делать это явно дешевле, чем полагаться на
    /// то, что все реализации ведут себя одинаково. Имя обрезается по размеру области.
    pub fn write_logger_name(&mut self, name: &[u16]) {
        let max_chars = NAME_BYTES / 2 - 1;
        let take = name.len().min(max_chars);
        let base = unsafe { self.words.as_mut_ptr().cast::<u8>().add(PROPS_SIZE).cast::<u16>() };
        for (i, &ch) in name.iter().take(take).enumerate() {
            unsafe { base.add(i).write_unaligned(ch) };
        }
        unsafe { base.add(take).write_unaligned(0) };
    }

    /// Имя сессии из буфера — его туда пишут `ControlTrace` и `QueryAllTraces`.
    pub fn logger_name(&self) -> String {
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.words.len() * 8)
        };
        let offset = unsafe { (*self.words.as_ptr().cast::<EVENT_TRACE_PROPERTIES>()).LoggerNameOffset }
            as usize;
        if offset == 0 || offset + 2 > bytes.len() {
            return String::new();
        }
        let available = (bytes.len() - offset) / 2;
        let chars: Vec<u16> = (0..available)
            .map(|i| u16::from_ne_bytes([bytes[offset + i * 2], bytes[offset + i * 2 + 1]]))
            .collect();
        from_wide(&chars)
    }
}

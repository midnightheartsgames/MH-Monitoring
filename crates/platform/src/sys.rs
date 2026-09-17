//! Мелочь, общая для обёрток Win32.

use std::fmt;

use windows_sys::Win32::Foundation::WIN32_ERROR;
use windows_sys::Win32::System::Diagnostics::Debug::{
    FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
};
use windows_sys::Win32::System::Diagnostics::Etw::EVENT_TRACE_PROPERTIES;

/// UTF-16 строка с завершающим нулём — в таком виде Win32 принимает `PCWSTR`/`PWSTR`.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Читает UTF-16 строку до первого нуля.
pub fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Читает UTF-16 строку с завершающим нулём по указателю. Нулевой указатель — пустая строка.
///
/// # Safety
/// `pointer` — нуль или строка, завершённая нулём и живая на время вызова.
pub unsafe fn from_wide_ptr(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut length = 0;
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
}

/// Системный текст ошибки.
///
/// Голый код вроде `5` в логе бесполезен, а различать «нет прав» и «сессия уже существует»
/// приходится постоянно (PLAN.md §2.3, §2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Win32Error(pub WIN32_ERROR);

impl fmt::Display for Win32Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buffer = [0u16; 512];
        let length = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                std::ptr::null(),
                self.0,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null(),
            )
        };
        let text = from_wide(&buffer[..length as usize]);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            write!(formatter, "код {}", self.0)
        } else {
            write!(formatter, "{text} (код {})", self.0)
        }
    }
}

impl std::error::Error for Win32Error {}

pub const PROPS_SIZE: usize = size_of::<EVENT_TRACE_PROPERTIES>();
/// Сколько байт резервируется под каждое из двух имён, лежащих за структурой.
pub const NAME_BYTES: usize = 1024;
pub const PROPS_BUFFER_BYTES: usize = PROPS_SIZE + 2 * NAME_BYTES;

/// Буфер под [`EVENT_TRACE_PROPERTIES`].
///
/// Две причины, по которым это не `Vec<u8>`:
///
/// * структура требует выравнивания на 8 байт, а `Vec<u8>` его не гарантирует;
/// * сразу за структурой в той же памяти лежат имя сессии и имя файла, на которые указывают
///   `LoggerNameOffset` и `LogFileNameOffset`, — выделять нужно больше, чем `size_of`.
pub struct PropsBuffer {
    words: Vec<u64>,
}

impl Default for PropsBuffer {
    fn default() -> Self {
        Self::new()
    }
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
    /// `with_log_file_name`: для realtime-сессии без файла `StartTrace` требует
    /// `LogFileNameOffset = 0`, а `ControlTrace` и `QueryAllTraces` — валидное смещение, куда
    /// они впишут имя.
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
    /// `StartTraceW` копирует имя туда сам, но делать это явно дешевле, чем полагаться на то,
    /// что все реализации ведут себя одинаково. Имя обрезается по размеру области.
    pub fn write_logger_name(&mut self, name: &[u16]) {
        let max_chars = NAME_BYTES / 2 - 1;
        let take = name.len().min(max_chars);
        let base = unsafe { self.words.as_mut_ptr().cast::<u8>().add(PROPS_SIZE).cast::<u16>() };
        for (index, &unit) in name.iter().take(take).enumerate() {
            unsafe { base.add(index).write_unaligned(unit) };
        }
        unsafe { base.add(take).write_unaligned(0) };
    }

    /// Имя сессии из буфера — его туда пишут `ControlTrace` и `QueryAllTraces`.
    pub fn logger_name(&self) -> String {
        let bytes = self.bytes();
        let offset =
            unsafe { (*self.words.as_ptr().cast::<EVENT_TRACE_PROPERTIES>()).LoggerNameOffset }
                as usize;
        if offset == 0 || offset + 2 > bytes.len() {
            return String::new();
        }
        let available = (bytes.len() - offset) / 2;
        let units: Vec<u16> = (0..available)
            .map(|index| {
                u16::from_ne_bytes([bytes[offset + index * 2], bytes[offset + index * 2 + 1]])
            })
            .collect();
        from_wide(&units)
    }

    fn bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.words.len() * 8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_round_trip() {
        let encoded = wide("MHMonitor-1234");
        assert_eq!(encoded.last(), Some(&0), "завершающий ноль обязателен");
        assert_eq!(from_wide(&encoded), "MHMonitor-1234");
    }

    #[test]
    fn a_name_survives_a_trip_through_the_properties_buffer() {
        let mut buffer = PropsBuffer::new();
        buffer.init(true);
        buffer.write_logger_name(&wide("MHMonitor-31504"));
        assert_eq!(buffer.logger_name(), "MHMonitor-31504");
    }

    #[test]
    fn the_buffer_is_larger_than_the_structure_itself() {
        let buffer = PropsBuffer::new();
        assert!(buffer.byte_len() > PROPS_SIZE, "за структурой обязано остаться место под имена");
    }

    /// Имя длиннее отведённой области обрезается, а не портит соседнюю память.
    #[test]
    fn an_overlong_name_is_truncated_not_overflowed() {
        let mut buffer = PropsBuffer::new();
        buffer.init(true);
        buffer.write_logger_name(&wide(&"x".repeat(NAME_BYTES)));
        assert_eq!(buffer.logger_name().chars().count(), NAME_BYTES / 2 - 1);
    }
}

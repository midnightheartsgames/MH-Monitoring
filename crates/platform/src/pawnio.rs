//! Связь с драйвером PawnIO (решение D4, PLAN.md §2.14).
//!
//! PawnIO — подписанный драйвер ядра, который выполняет только подписанные модули, а те отдают
//! наружу узкие вызовы вроде «прочитать такой-то MSR из белого списка». Через него читаются
//! температура и мощность CPU, недоступные из пользовательского режима.
//!
//! **С `PawnIOLib.dll` мы не линкуемся.** Драйвер распространяется под GPL-2.0 с исключением
//! только для программ, которые общаются с ним через интерфейс IOCTL устройства, а библиотека
//! лежит в том же GPL-репозитории. Поэтому протокол повторён здесь напрямую — он простой.

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_ABANDONED, WAIT_OBJECT_0, WIN32_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

use crate::sys::{Win32Error, wide};

/// Путь к устройству драйвера.
///
/// `\Device\PawnIO` в пространстве имён NT; `GLOBALROOT` делает его доступным для `CreateFileW`.
/// Старый DOS-путь `\\.\PawnIO` драйвер объявляет устаревшим.
const DEVICE_PATH: &str = r"\\?\GLOBALROOT\Device\PawnIO";

const DEVICE_TYPE: u32 = 41394;
const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;

/// Длина поля имени функции во входном буфере `IOCTL_PIO_EXECUTE_FN`.
const FN_NAME_LENGTH: usize = 32;

/// `CTL_CODE` из заголовков WDK.
const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

const IOCTL_PIO_LOAD_BINARY: u32 = ctl_code(DEVICE_TYPE, 0x821, METHOD_BUFFERED, FILE_ANY_ACCESS);
const IOCTL_PIO_EXECUTE_FN: u32 = ctl_code(DEVICE_TYPE, 0x841, METHOD_BUFFERED, FILE_ANY_ACCESS);

/// Почему PawnIO недоступен или вызов не удался.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PawnIoError {
    /// Драйвер не установлен: устройства нет. Пользователю нужен установщик с pawnio.eu.
    NotInstalled,
    /// Устройство есть, но открыть его не дали. Нужен администратор.
    AccessDenied,
    /// Имя функции не помещается в 32 байта протокола.
    NameTooLong,
    /// Драйвер вернул ошибку — модуль не подошёл к этому CPU, регистр не в белом списке и т. п.
    Driver(WIN32_ERROR),
}

impl std::fmt::Display for PawnIoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PawnIoError::NotInstalled => write!(formatter, "драйвер PawnIO не установлен"),
            PawnIoError::AccessDenied => {
                write!(formatter, "нет прав на драйвер PawnIO: нужен администратор")
            }
            PawnIoError::NameTooLong => write!(formatter, "имя функции модуля длиннее 31 байта"),
            PawnIoError::Driver(code) => write!(formatter, "PawnIO: {}", Win32Error(*code)),
        }
    }
}

impl std::error::Error for PawnIoError {}

/// Открытый дескриптор PawnIO с загруженным модулем.
///
/// Один дескриптор — один модуль: так устроен драйвер.
pub struct PawnIo {
    handle: HANDLE,
}

// Дескриптор устройства — непрозрачный номер ядра. Вызовы драйвера сериализует сам драйвер.
unsafe impl Send for PawnIo {}
unsafe impl Sync for PawnIo {}

impl PawnIo {
    /// Открывает драйвер и загружает в него подписанный модуль.
    pub fn open_with_module(module: &[u8]) -> Result<Self, PawnIoError> {
        let path = wide(DEVICE_PATH);
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(match unsafe { GetLastError() } {
                // ERROR_FILE_NOT_FOUND и ERROR_PATH_NOT_FOUND — устройства нет.
                2 | 3 => PawnIoError::NotInstalled,
                5 => PawnIoError::AccessDenied,
                other => PawnIoError::Driver(other),
            });
        }
        let pawnio = Self { handle };
        pawnio.ioctl(IOCTL_PIO_LOAD_BINARY, module, &mut [])?;
        Ok(pawnio)
    }

    /// Вызывает функцию модуля. Возвращает, сколько значений записано в `out`.
    pub fn execute(
        &self,
        name: &str,
        input: &[u64],
        out: &mut [u64],
    ) -> Result<usize, PawnIoError> {
        let request = encode_execute_request(name, input)?;
        let mut raw = vec![0u8; out.len() * 8];
        let written = self.ioctl(IOCTL_PIO_EXECUTE_FN, &request, &mut raw)?;
        Ok(decode_execute_response(&raw[..written], out))
    }

    fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize, PawnIoError> {
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                code,
                input.as_ptr().cast(),
                input.len() as u32,
                if output.is_empty() { std::ptr::null_mut() } else { output.as_mut_ptr().cast() },
                output.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(match unsafe { GetLastError() } {
                5 => PawnIoError::AccessDenied,
                other => PawnIoError::Driver(other),
            });
        }
        Ok(returned as usize)
    }
}

impl Drop for PawnIo {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Входной буфер `IOCTL_PIO_EXECUTE_FN`: имя функции в 32 байтах, дополненных нулями, затем
/// аргументы как `u64` в порядке x64.
fn encode_execute_request(name: &str, input: &[u64]) -> Result<Vec<u8>, PawnIoError> {
    // Последний байт поля обязан остаться нулём — драйвер читает имя как C-строку.
    if name.len() >= FN_NAME_LENGTH {
        return Err(PawnIoError::NameTooLong);
    }
    let mut request = vec![0u8; FN_NAME_LENGTH + input.len() * 8];
    request[..name.len()].copy_from_slice(name.as_bytes());
    for (index, value) in input.iter().enumerate() {
        let offset = FN_NAME_LENGTH + index * 8;
        request[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    Ok(request)
}

fn decode_execute_response(raw: &[u8], out: &mut [u64]) -> usize {
    let (words, _) = raw.as_chunks::<8>();
    let count = words.len().min(out.len());
    for (slot, word) in out.iter_mut().zip(words) {
        *slot = u64::from_le_bytes(*word);
    }
    count
}

/// Общесистемный мьютекс доступа к конфигурационному пространству PCI.
///
/// Пара регистров «индекс/данные», через которую читается SMN у AMD, одна на всю систему. Если
/// HWiNFO или LibreHardwareMonitor запущены одновременно, без этого мьютекса наш индекс может
/// перезаписать чужой, и обе программы прочитают мусор. Имя общее: так его называют и они.
pub struct PciAccessLock {
    handle: HANDLE,
}

unsafe impl Send for PciAccessLock {}
unsafe impl Sync for PciAccessLock {}

const PCI_MUTEX_NAME: &str = r"Global\Access_PCI";

impl PciAccessLock {
    pub fn open() -> Result<Self, PawnIoError> {
        let name = wide(PCI_MUTEX_NAME);
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(PawnIoError::Driver(unsafe { GetLastError() }));
        }
        Ok(Self { handle })
    }

    /// Выполняет `action` под мьютексом. `None`, если мьютекс не удалось взять за `timeout_ms`.
    pub fn with<T>(&self, timeout_ms: u32, action: impl FnOnce() -> T) -> Option<T> {
        let wait = unsafe { WaitForSingleObject(self.handle, timeout_ms) };
        // Брошенный мьютекс — владелец умер, не освободив его. Он теперь наш, и это не ошибка.
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            return None;
        }
        let result = action();
        unsafe { ReleaseMutex(self.handle) };
        Some(result)
    }
}

impl Drop for PciAccessLock {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Коды сверены с `CTL_CODE(41394, 0x821/0x841, METHOD_BUFFERED, FILE_ANY_ACCESS)` из
    /// `pawnio_um.h`.
    #[test]
    fn ioctl_codes_match_the_driver_header() {
        assert_eq!(IOCTL_PIO_LOAD_BINARY, (41394 << 16) | (0x821 << 2));
        assert_eq!(IOCTL_PIO_EXECUTE_FN, (41394 << 16) | (0x841 << 2));
        assert_eq!(IOCTL_PIO_EXECUTE_FN, 0xA1B2_2104);
    }

    #[test]
    fn a_request_is_a_padded_name_followed_by_arguments() {
        let request = encode_execute_request("ioctl_read_msr", &[0xC001_029B]).unwrap();
        assert_eq!(request.len(), 32 + 8);
        assert_eq!(&request[..14], b"ioctl_read_msr");
        assert!(request[14..32].iter().all(|&byte| byte == 0), "имя дополнено нулями");
        assert_eq!(u64::from_le_bytes(request[32..40].try_into().unwrap()), 0xC001_029B);
    }

    #[test]
    fn a_name_must_leave_room_for_the_terminator() {
        assert!(encode_execute_request(&"x".repeat(31), &[]).is_ok());
        assert_eq!(encode_execute_request(&"x".repeat(32), &[]), Err(PawnIoError::NameTooLong));
    }

    #[test]
    fn a_response_is_read_as_little_endian_words() {
        let mut raw = 7u64.to_le_bytes().to_vec();
        raw.extend_from_slice(&u64::MAX.to_le_bytes());
        let mut out = [0u64; 4];
        assert_eq!(decode_execute_response(&raw, &mut out), 2);
        assert_eq!(out[..2], [7, u64::MAX]);
    }

    #[test]
    fn a_short_output_buffer_is_not_overrun() {
        let raw = [1u8; 24];
        let mut out = [0u64; 1];
        assert_eq!(decode_execute_response(&raw, &mut out), 1);
    }

    /// Без драйвера — внятная причина, с драйвером без прав — тоже.
    #[test]
    fn opening_reports_a_clear_reason() {
        match PawnIo::open_with_module(&[]) {
            Ok(_) => panic!("пустой модуль драйвер принять не должен"),
            Err(error) => assert!(!error.to_string().is_empty()),
        }
    }

    #[test]
    fn the_pci_lock_can_be_taken_and_released() {
        let Ok(lock) = PciAccessLock::open() else {
            // Глобальный мьютекс может требовать прав на создание глобальных объектов.
            return;
        };
        assert_eq!(lock.with(100, || 42), Some(42));
        assert_eq!(lock.with(100, || 43), Some(43), "мьютекс освобождён после первого вызова");
    }
}

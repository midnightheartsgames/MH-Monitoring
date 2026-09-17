//! Именованный канал между службой и UI (PLAN.md §6/P6).
//!
//! Дескрипторы синхронные, и на них заблокированное чтение держит и запись. Поэтому обе стороны
//! сначала спрашивают [`PipeStream::available`] и читают только то, что уже пришло, — без
//! перекрывающегося ввода-вывода и без второго канала.

use std::io::{self, Read, Write};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
};

use crate::sys::wide;

/// `PIPE_ACCESS_DUPLEX`: в `windows-sys` константы под этим именем нет.
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const BUFFER_BYTES: u32 = 64 * 1024;
const SDDL_REVISION_1: u32 = 1;

/// Доступ для службы: SYSTEM и администраторы — полный, пользователи, вошедшие в систему
/// интерактивно, — чтение и запись. Удалённых клиентов канал не принимает вовсе.
pub const SERVICE_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

/// Один конец соединения.
pub struct PipeStream {
    handle: HANDLE,
}

// Дескриптор принадлежит одному владельцу; одновременного доступа нет — см. модульный комментарий.
unsafe impl Send for PipeStream {}

impl PipeStream {
    /// Подключается к каналу, дожидаясь свободного экземпляра не дольше `wait_ms`.
    pub fn connect(name: &str, wait_ms: u32) -> io::Result<PipeStream> {
        let name_w = wide(name);
        loop {
            let handle = unsafe {
                CreateFileW(
                    name_w.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(PipeStream { handle });
            }
            let error = unsafe { GetLastError() };
            if error != ERROR_PIPE_BUSY || unsafe { WaitNamedPipeW(name_w.as_ptr(), wait_ms) } == 0
            {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }
    }

    /// Сколько байт можно прочитать без блокировки. Ошибка — соединение разорвано.
    pub fn available(&self) -> io::Result<usize> {
        let mut available = 0u32;
        let ok = unsafe {
            PeekNamedPipe(
                self.handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(available as usize)
    }
}

impl Read for PipeStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                self.handle,
                buffer.as_mut_ptr(),
                buffer.len().min(u32::MAX as usize) as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            // Собеседник закрыл канал — для `Read` это конец потока, а не ошибка.
            if error == ERROR_BROKEN_PIPE {
                return Ok(0);
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        Ok(read as usize)
    }
}

impl Write for PipeStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                self.handle,
                buffer.as_ptr(),
                buffer.len().min(u32::MAX as usize) as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Не `FlushFileBuffers`: он ждёт, пока собеседник всё прочтёт, и один медленный клиент
        // остановил бы рассылку снимков.
        Ok(())
    }
}

impl Drop for PipeStream {
    /// Только закрытие. Ни `FlushFileBuffers` (ждёт, пока клиент всё прочтёт, — зависший клиент
    /// подвесил бы остановку службы), ни `DisconnectNamedPipe` (выбрасывает непрочитанное).
    /// Записанное до закрытия собеседник дочитывает, а затем видит конец потока.
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Серверная сторона: принимает клиентов по одному.
pub struct PipeListener {
    name: Vec<u16>,
    security: Option<SecurityDescriptor>,
    first_instance: bool,
}

// Дескриптор безопасности только читается.
unsafe impl Send for PipeListener {}
unsafe impl Sync for PipeListener {}

impl PipeListener {
    /// `sddl` — права на канал; `None` — права по умолчанию (для тестов).
    pub fn new(name: &str, sddl: Option<&str>) -> io::Result<PipeListener> {
        let security = sddl.map(SecurityDescriptor::from_sddl).transpose()?;
        Ok(PipeListener { name: wide(name), security, first_instance: true })
    }

    /// Ждёт клиента. Блокирует; разбудить — [`PipeListener::wake`].
    ///
    /// Первый экземпляр создаётся с `FILE_FLAG_FIRST_PIPE_INSTANCE`: если канал с этим именем уже
    /// создал кто-то другой, это ошибка, а не молчаливое соседство с чужим сервером.
    pub fn accept(&mut self) -> io::Result<PipeStream> {
        let attributes = self.security.as_ref().map(|security| SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0,
            bInheritHandle: 0,
        });
        let first = if self.first_instance { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        let handle = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                PIPE_ACCESS_DUPLEX | first,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER_BYTES,
                BUFFER_BYTES,
                0,
                attributes.as_ref().map_or(std::ptr::null(), |a| a as *const _),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        self.first_instance = false;
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if !connected {
            let error = io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(error);
        }
        Ok(PipeStream { handle })
    }

    /// Будит поток, заблокированный в [`PipeListener::accept`], — подключается и сразу уходит.
    pub fn wake(name: &str) {
        let _ = PipeStream::connect(name, 200);
    }
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> io::Result<SecurityDescriptor> {
        let sddl_w = wide(sddl);
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_w.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SecurityDescriptor(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_name(tag: &str) -> String {
        format!(r"\\.\pipe\MHMonitor-test-{}-{tag}", std::process::id())
    }

    #[test]
    fn a_client_and_a_server_exchange_bytes_both_ways() {
        let name = unique_name("exchange");
        let mut listener = PipeListener::new(&name, None).unwrap();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let mut buffer = [0u8; 5];
            stream.read_exact(&mut buffer).unwrap();
            assert_eq!(&buffer, b"hello");
            stream.write_all(b"world").unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut client = PipeStream::connect(&name, 2_000).unwrap();
        client.write_all(b"hello").unwrap();
        let mut buffer = [0u8; 5];
        client.read_exact(&mut buffer).unwrap();
        assert_eq!(&buffer, b"world");
        server.join().unwrap();
    }

    #[test]
    fn available_counts_pending_bytes_and_reports_a_closed_peer() {
        let name = unique_name("peek");
        let mut listener = PipeListener::new(&name, None).unwrap();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            stream.write_all(b"abc").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut client = PipeStream::connect(&name, 2_000).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(client.available().unwrap(), 3);
        let mut buffer = [0u8; 3];
        client.read_exact(&mut buffer).unwrap();
        server.join().unwrap();
        assert!(client.available().is_err(), "сервер ушёл — соединение разорвано");
        assert_eq!(client.read(&mut buffer).unwrap(), 0, "чтение видит конец потока");
    }

    #[test]
    fn connecting_to_a_missing_pipe_fails_fast() {
        let started = std::time::Instant::now();
        assert!(PipeStream::connect(&unique_name("missing"), 5_000).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_second_first_instance_is_refused() {
        let name = unique_name("squat");
        let mut first = PipeListener::new(&name, None).unwrap();
        let waiter = std::thread::spawn(move || first.accept().map(|_| ()));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut second = PipeListener::new(&name, None).unwrap();
        // Имя уже занято экземпляром первого слушателя.
        let squatted = std::thread::spawn(move || second.accept().map(|_| ()));
        std::thread::sleep(std::time::Duration::from_millis(50));
        PipeListener::wake(&name);
        assert!(waiter.join().unwrap().is_ok());
        assert!(squatted.join().unwrap().is_err());
    }

    #[test]
    fn the_service_sddl_parses() {
        assert!(SecurityDescriptor::from_sddl(SERVICE_PIPE_SDDL).is_ok());
        assert!(SecurityDescriptor::from_sddl("это не SDDL").is_err());
    }
}

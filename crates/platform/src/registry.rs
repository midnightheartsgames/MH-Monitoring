//! Строковые значения реестра — ровно столько, сколько нужно установке (PLAN.md §6/P7).

use std::io;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE,
    REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW,
    RegGetValueW, RegSetValueExW,
};

use crate::sys::wide;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    LocalMachine,
    CurrentUser,
}

impl Hive {
    fn root(self) -> HKEY {
        match self {
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
            Hive::CurrentUser => HKEY_CURRENT_USER,
        }
    }
}

/// Значение для записи.
pub enum Value<'a> {
    Text(&'a str),
    Number(u32),
}

fn check(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(status as i32)) }
}

/// Создаёт ключ, если его нет, и записывает значения.
pub fn write_values(hive: Hive, key: &str, values: &[(&str, Value<'_>)]) -> io::Result<()> {
    let key_w = wide(key);
    let mut handle: HKEY = std::ptr::null_mut();
    check(unsafe {
        RegCreateKeyExW(
            hive.root(),
            key_w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut handle,
            std::ptr::null_mut(),
        )
    })?;
    let result = values.iter().try_for_each(|(name, value)| {
        let name_w = wide(name);
        match value {
            Value::Text(text) => {
                let data = wide(text);
                check(unsafe {
                    RegSetValueExW(
                        handle,
                        name_w.as_ptr(),
                        0,
                        REG_SZ,
                        data.as_ptr().cast(),
                        (data.len() * 2) as u32,
                    )
                })
            }
            Value::Number(number) => check(unsafe {
                RegSetValueExW(
                    handle,
                    name_w.as_ptr(),
                    0,
                    REG_DWORD,
                    (number as *const u32).cast(),
                    4,
                )
            }),
        }
    });
    unsafe { RegCloseKey(handle) };
    result
}

/// Строковое значение; `None` — ключа или значения нет.
pub fn read_text(hive: Hive, key: &str, name: &str) -> Option<String> {
    let key_w = wide(key);
    let name_w = wide(name);
    let mut size = 0u32;
    let probe = unsafe {
        RegGetValueW(
            hive.root(),
            key_w.as_ptr(),
            name_w.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if probe != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    let status = unsafe {
        RegGetValueW(
            hive.root(),
            key_w.as_ptr(),
            name_w.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// Удаляет одно значение. Отсутствие значения — не ошибка.
pub fn delete_value(hive: Hive, key: &str, name: &str) -> io::Result<()> {
    let key_w = wide(key);
    let name_w = wide(name);
    let status = unsafe { RegDeleteKeyValueW(hive.root(), key_w.as_ptr(), name_w.as_ptr()) };
    if status == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(status) }
}

/// Удаляет ключ со всем содержимым. Отсутствие ключа — не ошибка.
pub fn delete_key(hive: Hive, key: &str) -> io::Result<()> {
    let key_w = wide(key);
    let status = unsafe { RegDeleteTreeW(hive.root(), key_w.as_ptr()) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(status)?;
    // `RegDeleteTreeW` удаляет содержимое, но не сам ключ.
    let status =
        unsafe { windows_sys::Win32::System::Registry::RegDeleteKeyW(hive.root(), key_w.as_ptr()) };
    if status == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(status) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пишем только в HKCU и только в свой тестовый ключ.
    #[test]
    fn values_round_trip_and_disappear() {
        let key = format!(r"Software\MHMonitorTests\{}", std::process::id());
        write_values(
            Hive::CurrentUser,
            &key,
            &[("Name", Value::Text("MH Monitoring — тест")), ("Size", Value::Number(42))],
        )
        .unwrap();
        assert_eq!(
            read_text(Hive::CurrentUser, &key, "Name").as_deref(),
            Some("MH Monitoring — тест")
        );
        assert_eq!(read_text(Hive::CurrentUser, &key, "Missing"), None);

        delete_value(Hive::CurrentUser, &key, "Name").unwrap();
        delete_value(Hive::CurrentUser, &key, "Name").unwrap();
        assert_eq!(read_text(Hive::CurrentUser, &key, "Name"), None);

        delete_key(Hive::CurrentUser, &key).unwrap();
        delete_key(Hive::CurrentUser, &key).unwrap();
        delete_key(Hive::CurrentUser, r"Software\MHMonitorTests").unwrap();
    }
}

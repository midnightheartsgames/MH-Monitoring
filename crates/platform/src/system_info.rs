//! Что Windows знает о процессоре и памяти без драйверов: классы ядер и таблица SMBIOS.

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::{
    GetLocalTime, GetLogicalProcessorInformationEx, GetSystemFirmwareTable, RSMB,
    RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

/// Местное время: часы и минуты.
pub fn local_time() -> (u8, u8) {
    let mut time: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut time) };
    (time.wHour as u8, time.wMinute as u8)
}

/// Класс эффективности каждого логического процессора, по порядку групп и номеров.
///
/// Пусто — Windows не ответила. У не гибридных процессоров у всех один класс.
pub fn efficiency_classes() -> Vec<u8> {
    let mut length = 0u32;
    unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut length)
    };
    if length == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u8; length as usize];
    let ok = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            buffer.as_mut_ptr().cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>(),
            &mut length,
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    buffer.truncate(length as usize);
    let mut processors = parse_cores(&buffer);
    processors.sort_unstable_by_key(|&(group, bit, _)| (group, bit));
    processors.into_iter().map(|(_, _, class)| class).collect()
}

/// Разбор ответа `GetLogicalProcessorInformationEx(RelationProcessorCore)`: для каждого
/// логического процессора — группа, номер в группе и класс его ядра.
///
/// Раскладка записи (x64): `Relationship: i32`, `Size: u32`, дальше `PROCESSOR_RELATIONSHIP`:
/// `Flags` @8, `EfficiencyClass` @9, `GroupCount: u16` @30, массив `GROUP_AFFINITY` @32 по 16 байт
/// (`Mask: usize`, `Group: u16`).
fn parse_cores(buffer: &[u8]) -> Vec<(u16, u8, u8)> {
    const CLASS: usize = 9;
    const GROUP_COUNT: usize = 30;
    const MASKS: usize = 32;
    const AFFINITY: usize = 16;

    let u16_at =
        |bytes: &[u8], at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |bytes: &[u8], at: usize| {
        bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let u64_at = |bytes: &[u8], at: usize| {
        bytes.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().expect("8 байт")))
    };

    let mut found = Vec::new();
    let mut offset = 0usize;
    while let Some(size) = u32_at(buffer, offset + 4) {
        let size = size as usize;
        let Some(record) = buffer.get(offset..offset + size).filter(|_| size >= MASKS) else {
            break;
        };
        let class = record[CLASS];
        let groups = u16_at(record, GROUP_COUNT).unwrap_or(0) as usize;
        for index in 0..groups {
            let at = MASKS + index * AFFINITY;
            let (Some(mask), Some(group)) = (u64_at(record, at), u16_at(record, at + 8)) else {
                break;
            };
            for bit in 0..64u8 {
                if mask & (1u64 << bit) != 0 {
                    found.push((group, bit, class));
                }
            }
        }
        offset += size;
    }
    found
}

/// Сырая таблица SMBIOS (`RawSMBIOSData`): 8 байт заголовка и сами структуры.
pub fn smbios_table() -> Option<Vec<u8>> {
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0) };
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    let written = unsafe { GetSystemFirmwareTable(RSMB, 0, buffer.as_mut_ptr(), size) };
    if written == 0 || written > size {
        return None;
    }
    buffer.truncate(written as usize);
    Some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(class: u8, masks: &[(u64, u16)]) -> Vec<u8> {
        let size = 32 + masks.len() * 16;
        let mut bytes = vec![0u8; size];
        bytes[4..8].copy_from_slice(&(size as u32).to_le_bytes());
        bytes[9] = class;
        bytes[30..32].copy_from_slice(&(masks.len() as u16).to_le_bytes());
        for (index, (mask, group)) in masks.iter().enumerate() {
            let at = 32 + index * 16;
            bytes[at..at + 8].copy_from_slice(&mask.to_le_bytes());
            bytes[at + 8..at + 10].copy_from_slice(&group.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn cores_are_expanded_to_logical_processors() {
        // P-ядро с двумя потоками и E-ядро с одним.
        let mut buffer = record(1, &[(0b11, 0)]);
        buffer.extend(record(0, &[(0b100, 0)]));
        assert_eq!(parse_cores(&buffer), vec![(0, 0, 1), (0, 1, 1), (0, 2, 0)]);
    }

    #[test]
    fn a_truncated_buffer_is_not_read_past_its_end() {
        let mut buffer = record(1, &[(1, 0)]);
        buffer.truncate(20);
        assert!(parse_cores(&buffer).is_empty());
    }

    #[test]
    fn this_machine_reports_a_class_for_every_processor() {
        let classes = efficiency_classes();
        let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
        assert_eq!(classes.len(), threads);
    }

    #[test]
    fn local_time_is_a_valid_clock_reading() {
        let (hour, minute) = local_time();
        assert!(hour < 24 && minute < 60);
    }

    #[test]
    fn this_machine_has_an_smbios_table() {
        let table = smbios_table().expect("SMBIOS есть на любой машине с UEFI или BIOS");
        assert!(table.len() > 8);
    }
}

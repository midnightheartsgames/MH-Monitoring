//! Частота памяти из таблицы SMBIOS — структуры 17 («Memory Device»).
//!
//! Берётся `Configured Memory Speed` — частота, на которой модуль работает сейчас (с XMP/EXPO —
//! разогнанная), а не паспортная `Speed`. Паспортная — запасной вариант для старых таблиц. У
//! разных модулей частота одна, поэтому достаточно наибольшей среди установленных.

/// Заголовок `RawSMBIOSData` перед структурами.
const RAW_HEADER: usize = 8;
const MEMORY_DEVICE: u8 = 17;
const END_OF_TABLE: u8 = 127;

// Смещения полей структуры 17.
const SIZE: usize = 0x0C;
const SPEED: usize = 0x15;
const CONFIGURED_SPEED: usize = 0x20;
const EXTENDED_SPEED: usize = 0x54;
const EXTENDED_CONFIGURED_SPEED: usize = 0x58;
/// Значение WORD, после которого частота читается из расширенного поля DWORD (SMBIOS 3.3).
const USE_EXTENDED: u16 = 0xFFFF;

/// Рабочая частота памяти, МГц. `None` — модулей нет или частоту таблица не сообщает.
pub fn memory_speed_mhz(raw: &[u8]) -> Option<u32> {
    structures(raw.get(RAW_HEADER..)?)
        .filter(|record| record.first() == Some(&MEMORY_DEVICE))
        .filter(|record| installed(record))
        .filter_map(|record| {
            speed(record, CONFIGURED_SPEED, EXTENDED_CONFIGURED_SPEED)
                .or_else(|| speed(record, SPEED, EXTENDED_SPEED))
        })
        .max()
}

/// Форматированные части структур таблицы, без строк.
fn structures(mut table: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let kind = *table.first()?;
        let length = *table.get(1)? as usize;
        if kind == END_OF_TABLE || length < 4 || table.len() < length {
            return None;
        }
        let record = &table[..length];
        // Строки идут после форматированной части и кончаются двумя нулями подряд.
        let strings = &table[length..];
        let end = strings.windows(2).position(|pair| pair == [0, 0])? + 2;
        table = &strings[end..];
        Some(record)
    })
}

/// Размер 0 — гнездо пустое; 0xFFFF — размер неизвестен, но модуль стоит.
fn installed(record: &[u8]) -> bool {
    word(record, SIZE).is_some_and(|size| size != 0)
}

fn speed(record: &[u8], at: usize, extended_at: usize) -> Option<u32> {
    let value = match word(record, at)? {
        0 => return None,
        USE_EXTENDED => dword(record, extended_at)?,
        value => u32::from(value),
    };
    (value > 0).then_some(value)
}

fn word(record: &[u8], at: usize) -> Option<u16> {
    record.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn dword(record: &[u8], at: usize) -> Option<u32> {
    record.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Структура с форматированной частью `length` байт и одной строкой.
    fn structure(kind: u8, length: usize, fields: &[(usize, &[u8])]) -> Vec<u8> {
        let mut bytes = vec![0u8; length];
        bytes[0] = kind;
        bytes[1] = length as u8;
        for (at, value) in fields {
            bytes[*at..*at + value.len()].copy_from_slice(value);
        }
        bytes.extend_from_slice(b"DIMM\0\0");
        bytes
    }

    fn table(structures: &[Vec<u8>]) -> Vec<u8> {
        let mut raw = vec![0u8; RAW_HEADER];
        for structure in structures {
            raw.extend_from_slice(structure);
        }
        raw.extend_from_slice(&[END_OF_TABLE, 4, 0, 0, 0, 0]);
        raw
    }

    fn module(size: u16, speed: u16, configured: u16) -> Vec<u8> {
        structure(
            MEMORY_DEVICE,
            0x28,
            &[
                (SIZE, &size.to_le_bytes()),
                (SPEED, &speed.to_le_bytes()),
                (CONFIGURED_SPEED, &configured.to_le_bytes()),
            ],
        )
    }

    #[test]
    fn the_configured_speed_wins_over_the_rated_one() {
        let raw = table(&[structure(0, 0x18, &[]), module(16_384, 4_800, 6_000)]);
        assert_eq!(memory_speed_mhz(&raw), Some(6_000));
    }

    #[test]
    fn empty_slots_are_ignored_and_the_rated_speed_is_a_fallback() {
        let raw = table(&[module(0, 5_600, 5_600), module(8_192, 3_200, 0)]);
        assert_eq!(memory_speed_mhz(&raw), Some(3_200));
    }

    #[test]
    fn a_large_speed_is_read_from_the_extended_field() {
        let record = structure(
            MEMORY_DEVICE,
            0x5C,
            &[
                (SIZE, &16_384u16.to_le_bytes()),
                (CONFIGURED_SPEED, &USE_EXTENDED.to_le_bytes()),
                (EXTENDED_CONFIGURED_SPEED, &70_000u32.to_le_bytes()),
            ],
        );
        assert_eq!(memory_speed_mhz(&table(&[record])), Some(70_000));
    }

    #[test]
    fn a_broken_table_gives_nothing() {
        assert_eq!(memory_speed_mhz(&[]), None);
        assert_eq!(memory_speed_mhz(&table(&[])), None);
        let mut raw = table(&[module(16_384, 4_800, 6_000)]);
        raw.truncate(RAW_HEADER + 10);
        assert_eq!(memory_speed_mhz(&raw), None);
    }
}

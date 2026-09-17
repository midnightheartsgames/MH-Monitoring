//! Расшифровка регистров температуры и энергии CPU Intel.
//!
//! Модуль чистый, как и [`super::amd`]. Формулы сверены с драйверами Linux
//! `drivers/hwmon/coretemp.c` и `arch/x86/events/rapl.c`. Регистры читаются через модуль PawnIO
//! `IntelMSR` (PLAN.md §2.14).

/// `MSR_IA32_TEMPERATURE_TARGET`: в битах 23:16 — TjMax.
pub const MSR_IA32_TEMPERATURE_TARGET: u64 = 0x1A2;
/// `MSR_IA32_PACKAGE_THERM_STATUS`: температура всего пакета.
pub const MSR_IA32_PACKAGE_THERM_STATUS: u64 = 0x1B1;
/// `MSR_IA32_THERM_STATUS`: температура ядра, на котором выполнилось чтение.
pub const MSR_IA32_THERM_STATUS: u64 = 0x19C;
/// `MSR_RAPL_POWER_UNIT`: единица счётчика энергии — те же биты 12:8, что у AMD.
pub const MSR_RAPL_POWER_UNIT: u64 = 0x606;
/// `MSR_PKG_ENERGY_STATUS`: счётчик энергии пакета, 32 бита.
pub const MSR_PKG_ENERGY_STATUS: u64 = 0x611;

/// TjMax, когда регистр его не сообщает. Так делает `coretemp` для современных процессоров.
const DEFAULT_TJ_MAX: f64 = 100.0;
/// Бит 31 `IA32_THERM_STATUS`: показание действительно.
const READING_VALID: u64 = 1 << 31;

/// Цифровой датчик (DTS) — CPUID.06H:EAX[0].
pub fn has_digital_sensor(leaf6_eax: u32) -> bool {
    leaf6_eax & 1 != 0
}

/// Датчик пакета (PTM) — CPUID.06H:EAX[6].
pub fn has_package_sensor(leaf6_eax: u32) -> bool {
    leaf6_eax & (1 << 6) != 0
}

/// TjMax в градусах: температура, от которой датчики отсчитывают запас.
pub fn tj_max(temperature_target: u64) -> f64 {
    let value = (temperature_target >> 16) & 0xFF;
    if value == 0 { DEFAULT_TJ_MAX } else { value as f64 }
}

/// Температура пакета. Датчик хранит не градусы, а запас до TjMax в битах 22:16.
pub fn package_temperature(status: u64, tj_max: f64) -> f64 {
    tj_max - ((status >> 16) & 0x7F) as f64
}

/// Температура ядра из `IA32_THERM_STATUS`. `None` — показание помечено недействительным.
pub fn core_temperature(status: u64, tj_max: f64) -> Option<f64> {
    (status & READING_VALID != 0).then_some(tj_max - ((status >> 16) & 0x7F) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tj_max_comes_from_bits_23_to_16() {
        assert_eq!(tj_max(0x0064_0000), 100.0);
        assert_eq!(tj_max(0x0069_0A00), 105.0);
        // Старшие и младшие биты не влияют.
        assert_eq!(tj_max(0xFF5A_FFFF), 90.0);
    }

    #[test]
    fn a_missing_tj_max_falls_back_to_100() {
        assert_eq!(tj_max(0), 100.0);
    }

    /// Запас 38 градусов при TjMax 100 — 62 °C.
    #[test]
    fn package_temperature_is_tj_max_minus_the_readout() {
        assert_eq!(package_temperature(38 << 16, 100.0), 62.0);
        assert_eq!(package_temperature((38 << 16) | 0x8800_0000 | 0xFF, 100.0), 62.0);
    }

    #[test]
    fn core_temperature_needs_the_valid_bit() {
        assert_eq!(core_temperature((25 << 16) | READING_VALID, 105.0), Some(80.0));
        assert_eq!(core_temperature(25 << 16, 105.0), None);
    }

    #[test]
    fn thermal_features_come_from_cpuid_leaf_6() {
        // Типичный современный Core: DTS, турбо, PTM и т. д.
        let eax = 0x0000_00F7;
        assert!(has_digital_sensor(eax));
        assert!(has_package_sensor(eax));
        assert!(!has_digital_sensor(0));
        assert!(!has_package_sensor(0x0000_0001));
    }
}

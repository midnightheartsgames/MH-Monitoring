//! Расшифровка регистров температуры и энергии CPU AMD семейств 17h–1Ah (Zen 1–5).
//!
//! Модуль чистый: он получает сырые значения регистров и отдаёт градусы и ватты. Формулы сверены
//! с драйверами Linux `drivers/hwmon/k10temp.c` и `arch/x86/events/rapl.c` — они читают те же
//! регистры и годами обкатаны на миллионах машин. Сами регистры читаются через PawnIO
//! (решение D4, PLAN.md §2.14).

use mh_core::Millis;

/// SMN-регистр текущей температуры (`ZEN_REPORTED_TEMP_CTRL_BASE` в `k10temp`).
pub const ZEN_REPORTED_TEMP_CTRL: u64 = 0x0005_9800;
/// `MSR_AMD_RAPL_POWER_UNIT` — в нём единица счётчика энергии.
pub const MSR_AMD_RAPL_POWER_UNIT: u64 = 0xC001_0299;
/// `MSR_AMD_PKG_ENERGY_STATUS` — счётчик энергии всего пакета.
pub const MSR_AMD_PKG_ENERGY_STATUS: u64 = 0xC001_029B;

const CUR_TEMP_SHIFT: u32 = 21;
const CUR_TEMP_RANGE_SEL: u32 = 1 << 19;
const CUR_TEMP_TJ_SEL: u32 = 0b11 << 16;
/// Сдвиг шкалы, который включают оба признака выше.
const RANGE_OFFSET_CELSIUS: f64 = 49.0;

/// Tctl в градусах Цельсия из регистра `ZEN_REPORTED_TEMP_CTRL`.
///
/// Как в `k10temp::get_raw_temp`: старшие 11 бит — температура с шагом 1/8 °C, и из неё
/// вычитается 49 °C, если установлен бит 19 **или** оба бита 17:16. Второе условие легко упустить,
/// а на части процессоров без него показания завышены на 49 градусов.
pub fn decode_tctl(register: u32) -> f64 {
    let mut celsius = f64::from(register >> CUR_TEMP_SHIFT) * 0.125;
    if register & CUR_TEMP_RANGE_SEL != 0 || register & CUR_TEMP_TJ_SEL == CUR_TEMP_TJ_SEL {
        celsius -= RANGE_OFFSET_CELSIUS;
    }
    celsius
}

/// Сдвиг Tctl относительно настоящей температуры кристалла у отдельных моделей.
///
/// Tctl — управляющее значение для вентиляторов, и первые Ryzen завышали его намеренно. Таблица
/// взята из `k10temp::tctl_offset_table` и относится только к семейству 17h; для Zen 3 и новее
/// сдвига нет, и Tctl совпадает с Tdie.
const TCTL_OFFSETS: &[(&str, f64)] = &[
    ("AMD Ryzen 5 1600X", 20.0),
    ("AMD Ryzen 7 1700X", 20.0),
    ("AMD Ryzen 7 1800X", 20.0),
    ("AMD Ryzen 7 2700X", 10.0),
    ("AMD Ryzen Threadripper 19", 27.0),
    ("AMD Ryzen Threadripper 29", 27.0),
];

pub fn tctl_offset(family: u32, brand: &str) -> f64 {
    if family != 0x17 {
        return 0.0;
    }
    TCTL_OFFSETS
        .iter()
        .find(|(model, _)| brand.contains(model))
        .map(|(_, offset)| *offset)
        .unwrap_or(0.0)
}

/// Цена одного деления счётчика энергии в джоулях.
///
/// Биты 12:8 регистра единиц задают степень двойки: деление стоит `1 / 2^n` джоуля (`rapl.c`).
pub fn energy_unit_joules(power_unit_register: u64) -> f64 {
    let exponent = ((power_unit_register >> 8) & 0x1F) as i32;
    1.0 / f64::from(2u32).powi(exponent)
}

/// Мощность, вычисленная по приросту счётчика энергии.
///
/// Счётчик 32-битный и переполняется: при 1/65536 Дж на деление это 65 536 Дж, то есть при
/// 150 Вт примерно раз в семь минут. Прирост поэтому считается по модулю 2^32.
#[derive(Debug, Clone)]
pub struct EnergyMeter {
    unit_joules: f64,
    previous: Option<(u32, Millis)>,
}

/// Порог правдоподобия: больше этого настольный пакет не потребляет. Значение выше — почти
/// наверняка два переполнения между опросами или испорченное чтение, а не реальная мощность.
const MAX_PLAUSIBLE_WATTS: f64 = 1_000.0;

impl EnergyMeter {
    pub fn new(unit_joules: f64) -> Self {
        Self { unit_joules, previous: None }
    }

    /// Новое показание счётчика. Мощность появляется со второго показания.
    ///
    /// `now_ms` — монотонное время снятия показания. Интервалы короче 50 мс не считаются: на них
    /// дискретность счётчика и планировщика даёт шум сильнее самого сигнала.
    pub fn update(&mut self, counter: u32, now_ms: Millis) -> Option<f64> {
        let Some((previous_counter, previous_ms)) = self.previous else {
            self.previous = Some((counter, now_ms));
            return None;
        };
        let elapsed_ms = now_ms.saturating_sub(previous_ms);
        if elapsed_ms < 50 {
            return None;
        }
        self.previous = Some((counter, now_ms));

        let delta = counter.wrapping_sub(previous_counter);
        let watts = f64::from(delta) * self.unit_joules / (elapsed_ms as f64 / 1_000.0);
        if watts.is_finite() && (0.0..=MAX_PLAUSIBLE_WATTS).contains(&watts) {
            Some(watts)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Регистр с температурой `celsius` и заданными признаками шкалы.
    fn register(celsius: f64, flags: u32) -> u32 {
        (((celsius * 8.0) as u32) << CUR_TEMP_SHIFT) | flags
    }

    #[test]
    fn plain_tctl_is_eighths_of_a_degree() {
        assert_eq!(decode_tctl(register(45.0, 0)), 45.0);
        assert_eq!(decode_tctl(register(62.625, 0)), 62.625);
    }

    #[test]
    fn range_select_shifts_the_scale_by_49_degrees() {
        // Регистр хранит 94 → на деле 45 °C.
        assert_eq!(decode_tctl(register(94.0, CUR_TEMP_RANGE_SEL)), 45.0);
    }

    /// Второе условие сдвига из `k10temp`, которое легко упустить.
    #[test]
    fn both_tj_select_bits_also_shift_the_scale() {
        assert_eq!(decode_tctl(register(94.0, CUR_TEMP_TJ_SEL)), 45.0);
        // Один из двух битов сдвига не включает.
        assert_eq!(decode_tctl(register(94.0, 1 << 16)), 94.0);
        assert_eq!(decode_tctl(register(94.0, 1 << 17)), 94.0);
    }

    #[test]
    fn the_shifted_scale_can_go_below_zero() {
        assert_eq!(decode_tctl(register(9.0, CUR_TEMP_RANGE_SEL)), -40.0);
    }

    #[test]
    fn the_low_bits_do_not_affect_the_temperature() {
        assert_eq!(decode_tctl(register(50.0, 0xFFFF)), 50.0);
    }

    #[test]
    fn early_ryzens_report_an_inflated_tctl() {
        assert_eq!(tctl_offset(0x17, "AMD Ryzen 7 1700X Eight-Core Processor"), 20.0);
        assert_eq!(tctl_offset(0x17, "AMD Ryzen 7 2700X Eight-Core Processor"), 10.0);
        assert_eq!(tctl_offset(0x17, "AMD Ryzen Threadripper 1950X 16-Core Processor"), 27.0);
    }

    #[test]
    fn modern_ryzens_need_no_tctl_offset() {
        assert_eq!(tctl_offset(0x19, "AMD Ryzen 9 5900X 12-Core Processor"), 0.0);
        assert_eq!(tctl_offset(0x17, "AMD Ryzen 7 1700 Eight-Core Processor"), 0.0);
        // Совпадение имени не в том семействе ничего не значит.
        assert_eq!(tctl_offset(0x19, "AMD Ryzen 7 1700X"), 0.0);
    }

    #[test]
    fn the_energy_unit_comes_from_bits_12_to_8() {
        // Типичное значение для Zen: 0x0A1003 — делитель 2^16.
        assert_eq!(energy_unit_joules(0x000A_1003), 1.0 / 65_536.0);
        assert_eq!(energy_unit_joules(0x0000_0E00), 1.0 / 16_384.0);
        assert_eq!(energy_unit_joules(0), 1.0);
    }

    const UNIT: f64 = 1.0 / 65_536.0;

    #[test]
    fn power_needs_two_readings() {
        let mut meter = EnergyMeter::new(UNIT);
        assert_eq!(meter.update(1_000, 0), None);
        let watts = meter.update(1_000 + 65_536 * 100, 1_000).expect("вторая точка даёт мощность");
        assert!((watts - 100.0).abs() < 1e-9, "watts = {watts}");
    }

    /// Переполнение 32-битного счётчика — штатный случай, а не скачок в минус.
    #[test]
    fn a_counter_wraparound_is_handled() {
        let mut meter = EnergyMeter::new(UNIT);
        let before_wrap = u32::MAX - 65_536 * 20 + 1;
        meter.update(before_wrap, 0);
        let watts = meter.update(65_536 * 30, 500).expect("переполнение не теряет показание");
        // Прирост 50 Дж за полсекунды — 100 Вт.
        assert!((watts - 100.0).abs() < 1e-6, "watts = {watts}");
    }

    #[test]
    fn a_too_short_interval_is_not_measured() {
        let mut meter = EnergyMeter::new(UNIT);
        meter.update(0, 1_000);
        assert_eq!(meter.update(65_536, 1_010), None, "10 мс — шум, а не измерение");
        // И опорная точка при этом не сдвинулась.
        let watts = meter.update(65_536 * 50, 1_500).unwrap();
        assert!((watts - 100.0).abs() < 1e-9, "watts = {watts}");
    }

    #[test]
    fn an_implausible_jump_is_rejected() {
        let mut meter = EnergyMeter::new(1.0);
        meter.update(0, 0);
        assert_eq!(meter.update(5_000, 1_000), None, "5 кВт — испорченное чтение");
    }

    #[test]
    fn idle_power_can_be_zero() {
        let mut meter = EnergyMeter::new(UNIT);
        meter.update(42, 0);
        assert_eq!(meter.update(42, 1_000), Some(0.0));
    }
}

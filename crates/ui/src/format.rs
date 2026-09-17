//! Форматирование чисел для HUD — только здесь, никогда в коде отрисовки.
//!
//! Перенесено из `Formatters.kt`. Память считается в двоичных единицах (ГиБ), а подписывается
//! «GB» — так её показывают все остальные мониторы; перевод сделан ровно в одном месте.
//! Единицы вывода выбирает пользователь ([`crate::units`]); значения на входе всегда в °C, МГц и
//! байтах.

use crate::units::{Frequency, Memory, Temperature};

/// Показывается там, где у датчика нет значения.
pub const EMPTY: &str = "—";
/// Показывается, пока не пришёл первый замер.
pub const WARMING_UP: &str = "…";

const GIB: f64 = (1u64 << 30) as f64;
const MIB: f64 = (1u64 << 20) as f64;

pub fn percent(value: f64) -> String {
    format!("{value:.0}%")
}

pub fn temperature(celsius: f64, unit: Temperature) -> String {
    match unit {
        Temperature::Celsius => format!("{celsius:.0}°C"),
        Temperature::Fahrenheit => format!("{:.0}°F", celsius * 9.0 / 5.0 + 32.0),
    }
}

/// В «Авто» 812 МГц остаются в MHz, 2812 становятся 2.81 GHz.
pub fn frequency(megahertz: f64, unit: Frequency) -> String {
    let in_ghz = match unit {
        Frequency::Auto => megahertz >= 1_000.0,
        Frequency::Mhz => false,
        Frequency::Ghz => true,
    };
    if in_ghz { format!("{:.2} GHz", megahertz / 1_000.0) } else { format!("{megahertz:.0} MHz") }
}

/// «7.2 / 16 GB»: единица один раз, круглый итог без дробной части. В мегабайтах — целые.
pub fn bytes_pair(used: Option<u64>, total: Option<u64>, unit: Memory) -> Option<String> {
    if used.is_none() && total.is_none() {
        return None;
    }
    let (used, total) = match unit {
        Memory::Gb => (
            used.map(|v| format!("{:.1}", v as f64 / GIB)),
            total.map(|v| {
                let gib = v as f64 / GIB;
                if gib >= 10.0 { format!("{gib:.0}") } else { format!("{gib:.1}") }
            }),
        ),
        Memory::Mb => (
            used.map(|v| format!("{:.0}", v as f64 / MIB)),
            total.map(|v| format!("{:.0}", v as f64 / MIB)),
        ),
    };
    let label = match unit {
        Memory::Gb => "GB",
        Memory::Mb => "MB",
    };
    let empty = || EMPTY.to_string();
    Some(format!("{} / {} {label}", used.unwrap_or_else(empty), total.unwrap_or_else(empty)))
}

/// «3.4 GB», а меньше гигабайта — «812 MB»: у многих игр и приложений десятые гигабайта ничего
/// не говорят.
pub fn bytes(value: u64, unit: Memory) -> String {
    let gib = value as f64 / GIB;
    if unit == Memory::Gb && gib >= 1.0 {
        format!("{gib:.1} GB")
    } else {
        format!("{:.0} MB", value as f64 / MIB)
    }
}

/// Имя устройства без того, что и так понятно в HUD: производителя видеокарты, значков ®/™ и
/// хвостов вроде «12-Core Processor». «NVIDIA GeForce RTX 5070 Ti» → «GeForce RTX 5070 Ti».
pub fn short_device_name(name: &str) -> String {
    let mut text = name.replace("(R)", "").replace("(TM)", "").replace(['®', '™'], "");
    for tail in [" CPU @", " with Radeon", " Processor"] {
        if let Some(at) = text.find(tail) {
            text.truncate(at);
        }
    }
    let words: Vec<&str> =
        text.split_whitespace().filter(|word| !word.ends_with("-Core")).collect();
    // У видеокарт производитель лишний («GeForce» и так говорит всё), у процессоров — часть
    // имени: «Ryzen 9» без «AMD» читается как обрывок.
    let skip = match words.as_slice() {
        [vendor, family, ..]
            if ["NVIDIA", "AMD", "Intel"].contains(vendor)
                && ["GeForce", "Radeon", "Arc"].contains(family) =>
        {
            1
        }
        _ => 0,
    };
    words[skip..].join(" ")
}

pub fn power(watts: f64) -> String {
    format!("{watts:.0} W")
}

pub fn rpm(value: f64) -> String {
    format!("{value:.0} RPM")
}

pub fn fps(value: f64) -> Option<String> {
    value.is_finite().then(|| format!("{value:.0}"))
}

pub fn frametime(milliseconds: f64) -> Option<String> {
    milliseconds.is_finite().then(|| format!("{milliseconds:.1} ms"))
}

/// «09:05».
pub fn clock((hour, minute): (u8, u8)) -> String {
    format!("{hour:02}:{minute:02}")
}

/// Задержка — целыми миллисекундами: десятые у неё шумят.
pub fn latency(milliseconds: f64) -> Option<String> {
    milliseconds.is_finite().then(|| format!("{milliseconds:.0} ms"))
}

/// Частота памяти: «6000 MHz».
pub fn memory_speed(megahertz: u32) -> String {
    format!("{megahertz} MHz")
}

/// Частота одного ядра в сетке: без единицы, она в подписи. В ГГц — две цифры после точки.
pub fn core_clock(megahertz: f64, unit: Frequency) -> String {
    match unit {
        Frequency::Mhz => format!("{megahertz:.0}"),
        Frequency::Auto | Frequency::Ghz => format!("{:.2}", megahertz / 1_000.0),
    }
}

/// «DXGI · 2560×1440», «DXGI» или «2560×1440» — что известно.
pub fn api_and_resolution(api: Option<&str>, resolution: Option<(u32, u32)>) -> Option<String> {
    let size = resolution.map(|(width, height)| format!("{width}×{height}"));
    match (api, size) {
        (Some(api), Some(size)) => Some(format!("{api} · {size}")),
        (Some(api), None) => Some(api.to_string()),
        (None, size) => size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_switches_units_at_a_thousand() {
        assert_eq!(frequency(812.0, Frequency::Auto), "812 MHz");
        assert_eq!(frequency(2_812.0, Frequency::Auto), "2.81 GHz");
        assert_eq!(frequency(4_600.0, Frequency::Auto), "4.60 GHz");
    }

    #[test]
    fn a_chosen_frequency_unit_is_kept() {
        assert_eq!(frequency(4_620.0, Frequency::Mhz), "4620 MHz");
        assert_eq!(frequency(812.0, Frequency::Ghz), "0.81 GHz");
    }

    #[test]
    fn memory_pairs_print_the_unit_once() {
        let gib = 1u64 << 30;
        let pair = |used, total| bytes_pair(used, total, Memory::Gb);
        assert_eq!(pair(Some(gib * 72 / 10), Some(gib * 16)).unwrap(), "7.2 / 16 GB");
        assert_eq!(pair(Some(gib), Some(gib * 8)).unwrap(), "1.0 / 8.0 GB");
        assert_eq!(pair(None, Some(gib * 16)).unwrap(), "— / 16 GB");
        assert_eq!(pair(None, None), None);
        assert_eq!(
            bytes_pair(Some(gib * 3 / 2), Some(gib * 16), Memory::Mb).unwrap(),
            "1536 / 16384 MB"
        );
    }

    #[test]
    fn a_single_amount_switches_to_megabytes_below_a_gigabyte() {
        assert_eq!(bytes(3 * (1 << 30) + (1 << 29), Memory::Gb), "3.5 GB");
        assert_eq!(bytes(812 * (1 << 20), Memory::Gb), "812 MB");
        assert_eq!(bytes(3 * (1 << 30), Memory::Mb), "3072 MB");
    }

    #[test]
    fn fahrenheit_is_converted() {
        assert_eq!(temperature(100.0, Temperature::Fahrenheit), "212°F");
        assert_eq!(temperature(66.0, Temperature::Fahrenheit), "151°F");
    }

    #[test]
    fn device_names_lose_the_obvious_parts() {
        assert_eq!(short_device_name("NVIDIA GeForce RTX 5070 Ti"), "GeForce RTX 5070 Ti");
        assert_eq!(short_device_name("AMD Radeon RX 7800 XT"), "Radeon RX 7800 XT");
        assert_eq!(short_device_name("Intel(R) Arc(TM) A770 Graphics"), "Arc A770 Graphics");
        assert_eq!(
            short_device_name("AMD Ryzen 9 5900X 12-Core Processor            "),
            "AMD Ryzen 9 5900X"
        );
        assert_eq!(
            short_device_name("Intel(R) Core(TM) i7-14700K CPU @ 3.40GHz"),
            "Intel Core i7-14700K"
        );
        assert_eq!(
            short_device_name("AMD Ryzen 7 5700G with Radeon Graphics"),
            "AMD Ryzen 7 5700G"
        );
        assert_eq!(short_device_name(""), "");
    }

    #[test]
    fn non_finite_fps_is_not_printed() {
        assert_eq!(fps(f64::INFINITY), None);
        assert_eq!(fps(143.6).unwrap(), "144");
        assert_eq!(frametime(f64::NAN), None);
        assert_eq!(frametime(6.94).unwrap(), "6.9 ms");
    }

    #[test]
    fn the_clock_has_leading_zeros() {
        assert_eq!(clock((9, 5)), "09:05");
        assert_eq!(clock((23, 59)), "23:59");
    }

    #[test]
    fn new_fps_rows() {
        assert_eq!(latency(26.6).unwrap(), "27 ms");
        assert_eq!(latency(f64::NAN), None);
        assert_eq!(
            api_and_resolution(Some("DXGI"), Some((2560, 1440))).unwrap(),
            "DXGI · 2560×1440"
        );
        assert_eq!(api_and_resolution(None, Some((800, 600))).unwrap(), "800×600");
        assert_eq!(api_and_resolution(None, None), None);
    }

    #[test]
    fn core_clocks_follow_the_unit() {
        assert_eq!(core_clock(4_620.0, Frequency::Auto), "4.62");
        assert_eq!(core_clock(4_620.0, Frequency::Mhz), "4620");
        assert_eq!(memory_speed(6_000), "6000 MHz");
    }

    #[test]
    fn simple_units() {
        assert_eq!(percent(42.4), "42%");
        assert_eq!(temperature(61.6, Temperature::Celsius), "62°C");
        assert_eq!(power(95.3), "95 W");
        assert_eq!(rpm(1_001.2), "1001 RPM");
    }
}

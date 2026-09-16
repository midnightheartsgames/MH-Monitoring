//! Форматирование чисел для HUD — только здесь, никогда в коде отрисовки.
//!
//! Перенесено из `Formatters.kt`. Память считается в двоичных единицах (ГиБ), а подписывается
//! «GB» — так её показывают все остальные мониторы; перевод сделан ровно в одном месте.

/// Показывается там, где у датчика нет значения.
pub const EMPTY: &str = "—";
/// Показывается, пока не пришёл первый замер.
pub const WARMING_UP: &str = "…";

const GIB: f64 = (1u64 << 30) as f64;

pub fn percent(value: f64) -> String {
    format!("{value:.0}%")
}

pub fn temperature(celsius: f64) -> String {
    format!("{celsius:.0}°C")
}

/// 812 МГц остаются в MHz, 2812 становятся 2.81 GHz.
pub fn frequency(megahertz: f64) -> String {
    if megahertz >= 1_000.0 {
        format!("{:.2} GHz", megahertz / 1_000.0)
    } else {
        format!("{megahertz:.0} MHz")
    }
}

/// «7.2 / 16 GB»: единица один раз, круглый итог без дробной части.
pub fn bytes_pair(used: Option<u64>, total: Option<u64>) -> Option<String> {
    if used.is_none() && total.is_none() {
        return None;
    }
    let used = used.map(|v| format!("{:.1}", v as f64 / GIB)).unwrap_or_else(|| EMPTY.into());
    let total = total
        .map(|v| {
            let gib = v as f64 / GIB;
            if gib >= 10.0 { format!("{gib:.0}") } else { format!("{gib:.1}") }
        })
        .unwrap_or_else(|| EMPTY.into());
    Some(format!("{used} / {total} GB"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_switches_units_at_a_thousand() {
        assert_eq!(frequency(812.0), "812 MHz");
        assert_eq!(frequency(2_812.0), "2.81 GHz");
        assert_eq!(frequency(4_600.0), "4.60 GHz");
    }

    #[test]
    fn memory_pairs_print_the_unit_once() {
        let gib = 1u64 << 30;
        assert_eq!(bytes_pair(Some(gib * 72 / 10), Some(gib * 16)).unwrap(), "7.2 / 16 GB");
        assert_eq!(bytes_pair(Some(gib), Some(gib * 8)).unwrap(), "1.0 / 8.0 GB");
        assert_eq!(bytes_pair(None, Some(gib * 16)).unwrap(), "— / 16 GB");
        assert_eq!(bytes_pair(None, None), None);
    }

    #[test]
    fn non_finite_fps_is_not_printed() {
        assert_eq!(fps(f64::INFINITY), None);
        assert_eq!(fps(143.6).unwrap(), "144");
        assert_eq!(frametime(f64::NAN), None);
        assert_eq!(frametime(6.94).unwrap(), "6.9 ms");
    }

    #[test]
    fn simple_units() {
        assert_eq!(percent(42.4), "42%");
        assert_eq!(temperature(61.6), "62°C");
        assert_eq!(power(95.3), "95 W");
        assert_eq!(rpm(1_001.2), "1001 RPM");
    }
}

//! Модель показаний железа и итоговый снимок, на который подписан UI.
//!
//! Перенесено из `HardwareStats.kt` и `MonitoringSnapshot.kt`. Все значения необязательные, и это
//! намеренно: отсутствующий датчик — нормальное состояние, а не ошибка. Ключевое правило всего
//! проекта — **UI подписан только на [`Snapshot`] и не знает, откуда взялось число** (PLAN.md §4).

use crate::Millis;
use crate::fps_state::FpsState;

/// Здоровье источника данных. Источник, который деградировал, не должен ронять приложение.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SensorStatus {
    /// Ещё не инициализирован.
    #[default]
    Unknown,
    /// Отдаёт всё, что обещает.
    Available,
    /// Работает, но часть датчиков на этой машине недоступна.
    Partial,
    /// Здесь непригоден: нет драйвера, нет процесса, не то железо.
    Unavailable,
    /// Сломался на ходу; значения считать устаревшими.
    Error,
}

impl SensorStatus {
    pub fn is_usable(self) -> bool {
        matches!(self, SensorStatus::Available | SensorStatus::Partial)
    }

    /// Насколько состояние «плохое». При слиянии тиров побеждает худшее, чтобы наполовину
    /// сломанный источник был виден как Partial или Error, а не прятался за исправным.
    pub fn severity(self) -> u8 {
        match self {
            SensorStatus::Available => 0,
            SensorStatus::Partial => 1,
            SensorStatus::Unknown => 2,
            SensorStatus::Unavailable => 3,
            SensorStatus::Error => 4,
        }
    }

    /// Худшее из двух.
    pub fn worst(self, other: SensorStatus) -> SensorStatus {
        if other.severity() > self.severity() { other } else { self }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GpuStats {
    pub name: Option<String>,
    pub load_percent: Option<f64>,
    pub temperature_c: Option<f64>,
    pub core_clock_mhz: Option<f64>,
    pub memory_clock_mhz: Option<f64>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
    pub power_watts: Option<f64>,
    pub fan_rpm: Option<f64>,
}

impl GpuStats {
    /// Ложь, когда машина не сообщает о GPU ничего, — секция может спрятать себя целиком.
    pub fn has_any_value(&self) -> bool {
        self.load_percent.is_some()
            || self.temperature_c.is_some()
            || self.core_clock_mhz.is_some()
            || self.vram_used_bytes.is_some()
            || self.power_watts.is_some()
            || self.fan_rpm.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuStats {
    pub name: Option<String>,
    pub load_percent: Option<f64>,
    /// Требует драйвера режима ядра, поэтому в чистом user-mode обычно `None` (PLAN.md §2.10).
    pub temperature_c: Option<f64>,
    pub clock_mhz: Option<f64>,
    pub power_watts: Option<f64>,
}

impl CpuStats {
    pub fn has_any_value(&self) -> bool {
        self.load_percent.is_some()
            || self.temperature_c.is_some()
            || self.clock_mhz.is_some()
            || self.power_watts.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryStats {
    pub used_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

impl MemoryStats {
    pub fn load_percent(&self) -> Option<f64> {
        let used = self.used_bytes?;
        let total = self.total_bytes?;
        if total == 0 {
            return None;
        }
        Some(used as f64 / total as f64 * 100.0)
    }

    pub fn has_any_value(&self) -> bool {
        self.used_bytes.is_some() || self.total_bytes.is_some()
    }
}

/// Единственное неизменяемое состояние, на которое подписан UI.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub gpu: GpuStats,
    pub cpu: CpuStats,
    pub memory: MemoryStats,
    /// Доступность, причина, цель и статистика одним куском: они не могут разойтись.
    pub fps: FpsState,
    pub hardware_status: SensorStatus,
    pub timestamp_ms: Millis,
}

impl Snapshot {
    /// Истина, пока не пришёл первый замер железа: в этом окне UI показывает «Определяю…».
    pub fn is_warming_up(&self) -> bool {
        self.hardware_status == SensorStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worst_status_wins() {
        assert_eq!(SensorStatus::Available.worst(SensorStatus::Error), SensorStatus::Error);
        assert_eq!(SensorStatus::Error.worst(SensorStatus::Available), SensorStatus::Error);
        assert_eq!(SensorStatus::Partial.worst(SensorStatus::Available), SensorStatus::Partial);
        // Unknown хуже Partial: «не знаю» честнее, чем «частично работает».
        assert_eq!(SensorStatus::Partial.worst(SensorStatus::Unknown), SensorStatus::Unknown);
    }

    #[test]
    fn usable_means_available_or_partial() {
        assert!(SensorStatus::Available.is_usable());
        assert!(SensorStatus::Partial.is_usable());
        assert!(!SensorStatus::Unknown.is_usable());
        assert!(!SensorStatus::Unavailable.is_usable());
        assert!(!SensorStatus::Error.is_usable());
    }

    #[test]
    fn memory_load_needs_both_numbers() {
        assert_eq!(MemoryStats { used_bytes: Some(50), total_bytes: None }.load_percent(), None);
        assert_eq!(MemoryStats { used_bytes: None, total_bytes: Some(100) }.load_percent(), None);
        let half = MemoryStats { used_bytes: Some(50), total_bytes: Some(100) };
        assert_eq!(half.load_percent(), Some(50.0));
    }

    /// Деление на ноль дало бы NaN или inf, а они дальше разъезжаются по всему UI.
    #[test]
    fn a_zero_total_is_not_a_hundred_percent() {
        let broken = MemoryStats { used_bytes: Some(50), total_bytes: Some(0) };
        assert_eq!(broken.load_percent(), None);
    }

    #[test]
    fn a_section_with_only_a_name_has_nothing_to_show() {
        let named = GpuStats { name: Some("RTX 5070 Ti".into()), ..Default::default() };
        assert!(!named.has_any_value(), "имя — не показание");
        let measured = GpuStats { load_percent: Some(42.0), ..named };
        assert!(measured.has_any_value());
    }

    #[test]
    fn a_fresh_snapshot_is_warming_up() {
        assert!(Snapshot::default().is_warming_up());
        let ready = Snapshot { hardware_status: SensorStatus::Available, ..Default::default() };
        assert!(!ready.is_warming_up());
    }
}

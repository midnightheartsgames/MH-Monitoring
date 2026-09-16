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

/// Почему секция показывает не всё.
///
/// Коды, а не свободный текст, — по тем же причинам, что и у кадров: одна ситуация обязана
/// читаться одинаково, и прочерк в HUD без объяснения запрещён планом (PLAN.md §6/P3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorReason {
    /// `nvml.dll` не найдена — драйвер NVIDIA не установлен.
    GpuDriverMissing,
    /// NVML есть, но видеокарты NVIDIA нет. AMD и Intel — задача ADLX и IGCL (решение D3).
    NoSupportedGpu,
    /// NVML ответил ошибкой.
    GpuQueryFailed,
    /// Для температуры и мощности CPU нужен драйвер PawnIO (решение D4).
    PawnIoMissing,
    /// Драйвер есть, но без прав администратора к нему не пускают.
    NeedsAdmin,
    /// Этот процессор пока не поддерживается для температуры и мощности.
    CpuNotSupported,
    /// Датчик не ответил.
    SensorReadFailed,
}

impl SensorReason {
    pub fn message(self) -> &'static str {
        match self {
            SensorReason::GpuDriverMissing => "драйвер NVIDIA не найден",
            SensorReason::NoSupportedGpu => "видеокарта не поддерживается",
            SensorReason::GpuQueryFailed => "видеокарта не отвечает",
            SensorReason::PawnIoMissing => "установите PawnIO для температуры и мощности",
            SensorReason::NeedsAdmin => "нужны права администратора",
            SensorReason::CpuNotSupported => "температура этого процессора не поддерживается",
            SensorReason::SensorReadFailed => "датчик не ответил",
        }
    }
}

impl std::fmt::Display for SensorReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Здоровье одной секции HUD: насколько она заполнена и почему не целиком.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SectionHealth {
    pub status: SensorStatus,
    pub reason: Option<SensorReason>,
}

impl SectionHealth {
    pub fn available() -> Self {
        Self { status: SensorStatus::Available, reason: None }
    }

    pub fn partial(reason: SensorReason) -> Self {
        Self { status: SensorStatus::Partial, reason: Some(reason) }
    }

    pub fn unavailable(reason: SensorReason) -> Self {
        Self { status: SensorStatus::Unavailable, reason: Some(reason) }
    }

    pub fn error(reason: SensorReason) -> Self {
        Self { status: SensorStatus::Error, reason: Some(reason) }
    }

    /// Слияние двух мнений о секции.
    ///
    /// `Unknown` здесь значит «нет мнения», а не «плохо»: тир опроса, который секцию вообще не
    /// читает, не должен тянуть её статус вниз. Из двух настоящих мнений побеждает худшее вместе
    /// со своей причиной.
    pub fn merge(self, other: SectionHealth) -> SectionHealth {
        match (self.status, other.status) {
            (SensorStatus::Unknown, _) => other,
            (_, SensorStatus::Unknown) => self,
            _ if other.status.severity() > self.status.severity() => other,
            _ => self,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GpuStats {
    pub health: SectionHealth,
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
    pub health: SectionHealth,
    pub name: Option<String>,
    pub load_percent: Option<f64>,
    /// Требует драйвера режима ядра — читается через PawnIO (решение D4, PLAN.md §2.14).
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
    pub health: SectionHealth,
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
        assert_eq!(
            MemoryStats { used_bytes: Some(50), total_bytes: None, ..Default::default() }
                .load_percent(),
            None
        );
        assert_eq!(
            MemoryStats { used_bytes: None, total_bytes: Some(100), ..Default::default() }
                .load_percent(),
            None
        );
        let half =
            MemoryStats { used_bytes: Some(50), total_bytes: Some(100), ..Default::default() };
        assert_eq!(half.load_percent(), Some(50.0));
    }

    /// Деление на ноль дало бы NaN или inf, а они дальше разъезжаются по всему UI.
    #[test]
    fn a_zero_total_is_not_a_hundred_percent() {
        let broken =
            MemoryStats { used_bytes: Some(50), total_bytes: Some(0), ..Default::default() };
        assert_eq!(broken.load_percent(), None);
    }

    #[test]
    fn a_section_with_only_a_name_has_nothing_to_show() {
        let named = GpuStats { name: Some("RTX 5070 Ti".into()), ..Default::default() };
        assert!(!named.has_any_value(), "имя — не показание");
        let measured = GpuStats { load_percent: Some(42.0), ..named };
        assert!(measured.has_any_value());
    }

    // --- здоровье секций ---

    #[test]
    fn unknown_is_no_opinion_when_merging() {
        let partial = SectionHealth::partial(SensorReason::PawnIoMissing);
        assert_eq!(SectionHealth::default().merge(partial), partial);
        assert_eq!(partial.merge(SectionHealth::default()), partial);
        assert_eq!(
            SectionHealth::default().merge(SectionHealth::default()).status,
            SensorStatus::Unknown
        );
    }

    /// Загрузка CPU читается всегда, температура — только с PawnIO. Секция обязана честно сказать
    /// «частично» и почему, а не выглядеть исправной.
    #[test]
    fn the_worse_opinion_wins_together_with_its_reason() {
        let merged =
            SectionHealth::available().merge(SectionHealth::partial(SensorReason::NeedsAdmin));
        assert_eq!(merged.status, SensorStatus::Partial);
        assert_eq!(merged.reason, Some(SensorReason::NeedsAdmin));

        let worse = SectionHealth::partial(SensorReason::PawnIoMissing)
            .merge(SectionHealth::error(SensorReason::GpuQueryFailed));
        assert_eq!(worse.reason, Some(SensorReason::GpuQueryFailed));
    }

    #[test]
    fn every_reason_has_a_message() {
        for reason in [
            SensorReason::GpuDriverMissing,
            SensorReason::NoSupportedGpu,
            SensorReason::GpuQueryFailed,
            SensorReason::PawnIoMissing,
            SensorReason::NeedsAdmin,
            SensorReason::CpuNotSupported,
            SensorReason::SensorReadFailed,
        ] {
            assert!(!reason.message().is_empty(), "{reason:?}");
        }
    }

    #[test]
    fn a_fresh_snapshot_is_warming_up() {
        assert!(Snapshot::default().is_warming_up());
        let ready = Snapshot { hardware_status: SensorStatus::Available, ..Default::default() };
        assert!(!ready.is_warming_up());
    }
}

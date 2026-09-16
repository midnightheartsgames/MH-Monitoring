//! Слияние замеров, приходящих с разной частотой, в один снимок.
//!
//! Перенесено из `MonitoringAggregator.kt` (PLAN.md §7.5). Два правила, ради которых он есть:
//!
//! * у каждого поля есть владеющий тир, поэтому быстрый замер никогда не затирает медленное
//!   показание пустотой;
//! * замер, который перестали обновлять, протухает — UI не может показывать замороженное
//!   значение вечно.
//!
//! Модуль намеренно синхронный и без состояния во времени, кроме переданных меток: это и есть
//! та часть, которую имеет смысл покрывать тестами.

use crate::Millis;
use crate::fps_state::FpsState;
use crate::telemetry::{CpuStats, GpuStats, MemoryStats, SensorStatus, Snapshot};

/// Сколько замер считается свежим.
pub const DEFAULT_STALE_AFTER_MS: Millis = 4_000;

/// Частота, с которой снимается группа показаний.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleTier {
    /// Частый опрос: загрузка и частоты.
    Load,
    /// Редкий опрос: температуры, питание, объёмы памяти, имена.
    Slow,
}

/// Одна порция показаний от источника железа.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HardwareSample {
    pub gpu: GpuStats,
    pub cpu: CpuStats,
    pub memory: MemoryStats,
    pub status: SensorStatus,
}

#[derive(Debug, Clone)]
pub struct Aggregator {
    load: Option<HardwareSample>,
    load_at_ms: Millis,
    slow: Option<HardwareSample>,
    slow_at_ms: Millis,
    fps: FpsState,
    stale_after_ms: Millis,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Aggregator {
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_STALE_AFTER_MS)
    }

    pub fn with_ttl(stale_after_ms: Millis) -> Self {
        Self {
            load: None,
            load_at_ms: 0,
            slow: None,
            slow_at_ms: 0,
            fps: FpsState::INITIAL,
            stale_after_ms,
        }
    }

    pub fn submit_hardware(&mut self, tier: SampleTier, sample: HardwareSample, now_ms: Millis) {
        match tier {
            SampleTier::Load => {
                self.load = Some(sample);
                self.load_at_ms = now_ms;
            }
            SampleTier::Slow => {
                self.slow = Some(sample);
                self.slow_at_ms = now_ms;
            }
        }
    }

    /// Источник кадров публикует одно состояние целиком, поэтому смена статуса без новых кадров
    /// доходит до UI так же надёжно, как новый набор цифр.
    pub fn submit_fps(&mut self, state: FpsState) {
        self.fps = state;
    }

    pub fn snapshot(&self, now_ms: Millis) -> Snapshot {
        let fast = self.fresh(self.load.as_ref(), self.load_at_ms, now_ms);
        let slow = self.fresh(self.slow.as_ref(), self.slow_at_ms, now_ms);

        Snapshot {
            gpu: merge_gpu(fast.map(|s| &s.gpu), slow.map(|s| &s.gpu)),
            cpu: merge_cpu(fast.map(|s| &s.cpu), slow.map(|s| &s.cpu)),
            memory: MemoryStats {
                health: merge_health(fast.map(|s| s.memory.health), slow.map(|s| s.memory.health)),
                // Объёмы памяти — поле медленного тира; быстрый лишь подстраховывает.
                used_bytes: pick(
                    slow.and_then(|s| s.memory.used_bytes),
                    fast.and_then(|s| s.memory.used_bytes),
                ),
                total_bytes: pick(
                    slow.and_then(|s| s.memory.total_bytes),
                    fast.and_then(|s| s.memory.total_bytes),
                ),
            },
            fps: self.fps.clone(),
            hardware_status: merge_status(fast.map(|s| s.status), slow.map(|s| s.status)),
            timestamp_ms: now_ms,
        }
    }

    fn fresh<'a>(
        &self,
        sample: Option<&'a HardwareSample>,
        at_ms: Millis,
        now_ms: Millis,
    ) -> Option<&'a HardwareSample> {
        // saturating_sub, потому что часы монотонные, но метка может оказаться «из будущего»
        // после того, как аггрегатор пересоздали, а время пошло дальше.
        sample.filter(|_| now_ms.saturating_sub(at_ms) <= self.stale_after_ms)
    }
}

/// Владеющий тир идёт первым: его значение выигрывает, второй только подстраховывает.
fn pick<T>(owner: Option<T>, fallback: Option<T>) -> Option<T> {
    owner.or(fallback)
}

/// Здоровье секции по обоим тирам. Протухший или отсутствующий тир мнения не имеет.
fn merge_health(
    fast: Option<crate::telemetry::SectionHealth>,
    slow: Option<crate::telemetry::SectionHealth>,
) -> crate::telemetry::SectionHealth {
    fast.unwrap_or_default().merge(slow.unwrap_or_default())
}

fn merge_gpu(fast: Option<&GpuStats>, slow: Option<&GpuStats>) -> GpuStats {
    GpuStats {
        health: merge_health(fast.map(|s| s.health), slow.map(|s| s.health)),
        name: pick(slow.and_then(|s| s.name.clone()), fast.and_then(|s| s.name.clone())),
        load_percent: pick(fast.and_then(|s| s.load_percent), slow.and_then(|s| s.load_percent)),
        temperature_c: pick(slow.and_then(|s| s.temperature_c), fast.and_then(|s| s.temperature_c)),
        core_clock_mhz: pick(
            fast.and_then(|s| s.core_clock_mhz),
            slow.and_then(|s| s.core_clock_mhz),
        ),
        memory_clock_mhz: pick(
            fast.and_then(|s| s.memory_clock_mhz),
            slow.and_then(|s| s.memory_clock_mhz),
        ),
        vram_used_bytes: pick(
            slow.and_then(|s| s.vram_used_bytes),
            fast.and_then(|s| s.vram_used_bytes),
        ),
        vram_total_bytes: pick(
            slow.and_then(|s| s.vram_total_bytes),
            fast.and_then(|s| s.vram_total_bytes),
        ),
        power_watts: pick(slow.and_then(|s| s.power_watts), fast.and_then(|s| s.power_watts)),
        fan_rpm: pick(slow.and_then(|s| s.fan_rpm), fast.and_then(|s| s.fan_rpm)),
    }
}

fn merge_cpu(fast: Option<&CpuStats>, slow: Option<&CpuStats>) -> CpuStats {
    CpuStats {
        health: merge_health(fast.map(|s| s.health), slow.map(|s| s.health)),
        name: pick(slow.and_then(|s| s.name.clone()), fast.and_then(|s| s.name.clone())),
        load_percent: pick(fast.and_then(|s| s.load_percent), slow.and_then(|s| s.load_percent)),
        temperature_c: pick(slow.and_then(|s| s.temperature_c), fast.and_then(|s| s.temperature_c)),
        clock_mhz: pick(fast.and_then(|s| s.clock_mhz), slow.and_then(|s| s.clock_mhz)),
        power_watts: pick(slow.and_then(|s| s.power_watts), fast.and_then(|s| s.power_watts)),
    }
}

fn merge_status(fast: Option<SensorStatus>, slow: Option<SensorStatus>) -> SensorStatus {
    match (fast, slow) {
        (Some(a), Some(b)) => a.worst(b),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => SensorStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fps_state::{FpsAvailability, FpsReason};

    fn load_sample() -> HardwareSample {
        HardwareSample {
            gpu: GpuStats {
                load_percent: Some(75.0),
                core_clock_mhz: Some(2400.0),
                ..Default::default()
            },
            cpu: CpuStats {
                load_percent: Some(30.0),
                clock_mhz: Some(4200.0),
                ..Default::default()
            },
            memory: MemoryStats::default(),
            status: SensorStatus::Available,
        }
    }

    fn slow_sample() -> HardwareSample {
        HardwareSample {
            gpu: GpuStats {
                name: Some("RTX 5070 Ti".into()),
                temperature_c: Some(64.0),
                vram_used_bytes: Some(8_000),
                vram_total_bytes: Some(16_000),
                power_watts: Some(210.0),
                ..Default::default()
            },
            cpu: CpuStats {
                name: Some("Ryzen".into()),
                temperature_c: Some(55.0),
                ..Default::default()
            },
            memory: MemoryStats {
                used_bytes: Some(16_000),
                total_bytes: Some(32_000),
                ..Default::default()
            },
            status: SensorStatus::Available,
        }
    }

    #[test]
    fn tiers_merge_field_by_field() {
        let mut aggregator = Aggregator::new();
        aggregator.submit_hardware(SampleTier::Load, load_sample(), 1_000);
        aggregator.submit_hardware(SampleTier::Slow, slow_sample(), 1_000);

        let snapshot = aggregator.snapshot(1_000);
        assert_eq!(snapshot.gpu.load_percent, Some(75.0), "загрузка — от быстрого тира");
        assert_eq!(snapshot.gpu.temperature_c, Some(64.0), "температура — от медленного");
        assert_eq!(snapshot.gpu.name.as_deref(), Some("RTX 5070 Ti"));
        assert_eq!(snapshot.cpu.clock_mhz, Some(4200.0));
        assert_eq!(snapshot.memory.load_percent(), Some(50.0));
    }

    /// То, ради чего у полей есть владеющий тир: частый замер не знает температуры, и он не
    /// должен обнулять её на каждом опросе.
    #[test]
    fn a_fast_sample_never_erases_a_slow_reading() {
        let mut aggregator = Aggregator::new();
        aggregator.submit_hardware(SampleTier::Slow, slow_sample(), 1_000);
        for tick in 1..=5 {
            aggregator.submit_hardware(SampleTier::Load, load_sample(), 1_000 + tick * 250);
            let snapshot = aggregator.snapshot(1_000 + tick * 250);
            assert_eq!(snapshot.gpu.temperature_c, Some(64.0), "тик {tick}");
            assert_eq!(snapshot.gpu.vram_total_bytes, Some(16_000), "тик {tick}");
        }
    }

    // --- TTL ---

    #[test]
    fn a_sample_stays_until_the_ttl_expires() {
        let mut aggregator = Aggregator::with_ttl(4_000);
        aggregator.submit_hardware(SampleTier::Load, load_sample(), 1_000);

        assert_eq!(aggregator.snapshot(4_999).gpu.load_percent, Some(75.0));
        assert_eq!(
            aggregator.snapshot(5_000).gpu.load_percent,
            Some(75.0),
            "ровно TTL — ещё свежо"
        );
        assert_eq!(aggregator.snapshot(5_001).gpu.load_percent, None, "секундой позже — протухло");
    }

    /// Смысл TTL: замерший источник обязан перестать показывать цифры, а не держать последние.
    #[test]
    fn an_expired_tier_disappears_from_the_snapshot() {
        let mut aggregator = Aggregator::with_ttl(4_000);
        aggregator.submit_hardware(SampleTier::Load, load_sample(), 1_000);
        aggregator.submit_hardware(SampleTier::Slow, slow_sample(), 1_000);

        // Медленный тир продолжает работать, быстрый замолчал.
        aggregator.submit_hardware(SampleTier::Slow, slow_sample(), 10_000);
        let snapshot = aggregator.snapshot(10_000);

        assert_eq!(snapshot.gpu.load_percent, None, "загрузки больше нет");
        assert_eq!(snapshot.gpu.temperature_c, Some(64.0), "температура жива");
        assert_eq!(snapshot.gpu.core_clock_mhz, None);
    }

    #[test]
    fn when_every_tier_expires_the_status_returns_to_unknown() {
        let mut aggregator = Aggregator::with_ttl(1_000);
        aggregator.submit_hardware(SampleTier::Load, load_sample(), 0);
        aggregator.submit_hardware(SampleTier::Slow, slow_sample(), 0);

        let snapshot = aggregator.snapshot(50_000);
        assert_eq!(snapshot.hardware_status, SensorStatus::Unknown);
        assert!(snapshot.is_warming_up());
        assert!(!snapshot.gpu.has_any_value());
    }

    #[test]
    fn the_worst_status_of_the_live_tiers_wins() {
        let mut aggregator = Aggregator::new();
        aggregator.submit_hardware(SampleTier::Load, load_sample(), 1_000);
        aggregator.submit_hardware(
            SampleTier::Slow,
            HardwareSample { status: SensorStatus::Error, ..slow_sample() },
            1_000,
        );
        assert_eq!(aggregator.snapshot(1_000).hardware_status, SensorStatus::Error);
    }

    // --- здоровье секций ---

    /// Быстрый тир знает только загрузку CPU и молчит о температуре, медленный говорит, что
    /// температуры нет без PawnIO. Итог — «частично» с причиной, а не «исправно».
    #[test]
    fn section_health_merges_across_tiers() {
        use crate::telemetry::{SectionHealth, SensorReason};

        let mut aggregator = Aggregator::new();
        let mut fast = load_sample();
        fast.cpu.health = SectionHealth::available();
        let mut slow = slow_sample();
        slow.cpu.health = SectionHealth::partial(SensorReason::PawnIoMissing);
        aggregator.submit_hardware(SampleTier::Load, fast, 1_000);
        aggregator.submit_hardware(SampleTier::Slow, slow, 1_000);

        let health = aggregator.snapshot(1_000).cpu.health;
        assert_eq!(health.status, SensorStatus::Partial);
        assert_eq!(health.reason, Some(SensorReason::PawnIoMissing));
    }

    /// Тир, который секцию не опрашивает, её статус не портит.
    #[test]
    fn a_tier_that_ignores_a_section_does_not_degrade_it() {
        use crate::telemetry::SectionHealth;

        let mut aggregator = Aggregator::new();
        let mut fast = load_sample();
        fast.memory.health = SectionHealth::available();
        let slow = slow_sample(); // здоровье памяти не задано — Unknown
        aggregator.submit_hardware(SampleTier::Load, fast, 1_000);
        aggregator.submit_hardware(SampleTier::Slow, slow, 1_000);

        assert_eq!(aggregator.snapshot(1_000).memory.health.status, SensorStatus::Available);
    }

    // --- FPS ---

    /// Ради этого состояние кадров и лежит одним значением: смена статуса без новых цифр
    /// обязана дойти до снимка.
    #[test]
    fn a_status_change_without_new_frames_still_reaches_the_snapshot() {
        let mut aggregator = Aggregator::new();
        aggregator
            .submit_fps(FpsState { availability: FpsAvailability::Available, ..FpsState::INITIAL });
        assert!(aggregator.snapshot(1_000).fps.is_delivering());

        aggregator.submit_fps(FpsState {
            availability: FpsAvailability::Error,
            reason: Some(FpsReason::BackendFailed),
            ..FpsState::INITIAL
        });
        let snapshot = aggregator.snapshot(1_000);
        assert!(!snapshot.fps.is_delivering());
        assert_eq!(snapshot.fps.reason, Some(FpsReason::BackendFailed));
    }

    /// У кадров свой жизненный цикл: они не привязаны к TTL тиров железа.
    #[test]
    fn fps_state_does_not_expire_with_the_hardware_tiers() {
        let mut aggregator = Aggregator::with_ttl(1_000);
        aggregator.submit_hardware(SampleTier::Load, load_sample(), 0);
        aggregator
            .submit_fps(FpsState { availability: FpsAvailability::Available, ..FpsState::INITIAL });

        let snapshot = aggregator.snapshot(50_000);
        assert_eq!(snapshot.gpu.load_percent, None, "железо протухло");
        assert!(snapshot.fps.is_delivering(), "а состояние кадров — нет");
    }

    #[test]
    fn an_empty_aggregator_yields_a_warming_up_snapshot() {
        let snapshot = Aggregator::new().snapshot(1_000);
        assert!(snapshot.is_warming_up());
        assert_eq!(snapshot.timestamp_ms, 1_000);
        assert_eq!(snapshot.fps, FpsState::INITIAL);
    }
}

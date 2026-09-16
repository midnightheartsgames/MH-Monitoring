//! Загрузка и частота CPU, объём памяти — всё, что ОС отдаёт без драйверов.

use mh_core::{CpuStats, MemoryStats, SectionHealth};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

pub struct SystemSensor {
    system: System,
    /// Живая частота из счётчиков Windows. `None` — счётчиков нет, берём частоту из sysinfo.
    #[cfg(windows)]
    live_frequency: Option<mh_platform::pdh::CounterQuery>,
    /// Первое чтение загрузки у sysinfo всегда 0 %: ей нужна пара замеров. Его не показываем.
    usage_primed: bool,
}

impl Default for SystemSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemSensor {
    pub fn new() -> Self {
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage().with_frequency())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        Self {
            system,
            #[cfg(windows)]
            live_frequency: open_live_frequency(),
            usage_primed: false,
        }
    }

    /// Частый тир: загрузка и средняя частота ядер.
    pub fn read_load(&mut self) -> CpuStats {
        self.system
            .refresh_cpu_specifics(CpuRefreshKind::nothing().with_cpu_usage().with_frequency());
        let load_percent = if self.usage_primed {
            Some(f64::from(self.system.global_cpu_usage()).clamp(0.0, 100.0))
        } else {
            self.usage_primed = true;
            None
        };
        let frequencies: Vec<u64> = self.system.cpus().iter().map(|cpu| cpu.frequency()).collect();
        let clock_mhz = self.read_live_frequency().or_else(|| average_mhz(&frequencies));
        CpuStats {
            // Пока загрузка не готова, секция ничего не утверждает — «нет мнения», а не «сломано».
            health: if load_percent.is_some() {
                SectionHealth::available()
            } else {
                Default::default()
            },
            load_percent,
            clock_mhz,
            ..Default::default()
        }
    }

    #[cfg(windows)]
    fn read_live_frequency(&mut self) -> Option<f64> {
        let query = self.live_frequency.as_mut()?;
        query.collect().ok()?;
        live_mhz(query.value(FREQUENCY)?, query.value(PERFORMANCE)?)
    }

    #[cfg(not(windows))]
    fn read_live_frequency(&mut self) -> Option<f64> {
        None
    }

    /// Редкий тир: имя процессора и память.
    pub fn read_slow(&mut self) -> (CpuStats, MemoryStats) {
        self.system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        let name = self
            .system
            .cpus()
            .first()
            .map(|cpu| cpu.brand().trim().to_string())
            .filter(|brand| !brand.is_empty());
        let total = self.system.total_memory();
        let memory = if total > 0 {
            MemoryStats {
                health: SectionHealth::available(),
                used_bytes: Some(self.system.used_memory()),
                total_bytes: Some(total),
            }
        } else {
            MemoryStats::default()
        };
        (CpuStats { name, ..Default::default() }, memory)
    }
}

#[cfg(windows)]
const PERFORMANCE: usize = 0;
#[cfg(windows)]
const FREQUENCY: usize = 1;

/// Частота из sysinfo под Windows — номинальная и буста не видит (на 5900X 3701 МГц при реальных
/// ~4600). Живая частота — это номинальная, умноженная на `% Processor Performance`: так считает
/// диспетчер задач.
#[cfg(windows)]
fn open_live_frequency() -> Option<mh_platform::pdh::CounterQuery> {
    let mut query = mh_platform::pdh::CounterQuery::open(&[
        r"\Processor Information(_Total)\% Processor Performance",
        r"\Processor Information(_Total)\Processor Frequency",
    ])
    .ok()?;
    // Счётчик-скорость: первый замер только запоминает точку отсчёта.
    query.collect().ok()?;
    Some(query)
}

/// Номинальная частота × производительность в процентах. Производительность выше 100 — это буст.
#[cfg_attr(not(windows), allow(dead_code))]
fn live_mhz(nominal_mhz: f64, performance_percent: f64) -> Option<f64> {
    (nominal_mhz > 0.0 && performance_percent > 0.0)
        .then_some(nominal_mhz * performance_percent / 100.0)
}

/// Средняя частота. Нули — ядра, о которых ОС ничего не сказала; они не должны тянуть среднее вниз.
fn average_mhz(frequencies: &[u64]) -> Option<f64> {
    let known: Vec<u64> = frequencies.iter().copied().filter(|&mhz| mhz > 0).collect();
    if known.is_empty() {
        return None;
    }
    Some(known.iter().sum::<u64>() as f64 / known.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_cores_do_not_drag_the_average_down() {
        assert_eq!(average_mhz(&[3_600, 0, 4_000]), Some(3_800.0));
        assert_eq!(average_mhz(&[0, 0]), None);
        assert_eq!(average_mhz(&[]), None);
    }

    #[test]
    fn boost_raises_the_frequency_above_nominal() {
        assert_eq!(live_mhz(3_700.0, 125.0), Some(4_625.0));
        assert_eq!(live_mhz(3_700.0, 50.0), Some(1_850.0));
        assert_eq!(live_mhz(0.0, 125.0), None);
        assert_eq!(live_mhz(3_700.0, 0.0), None);
    }

    #[test]
    fn the_first_load_reading_is_withheld() {
        let mut sensor = SystemSensor::new();
        let first = sensor.read_load();
        assert_eq!(first.load_percent, None);
        assert_eq!(first.health, SectionHealth::default());

        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let second = sensor.read_load();
        let load = second.load_percent.expect("со второго раза загрузка есть");
        assert!((0.0..=100.0).contains(&load));
    }

    #[test]
    fn this_machine_reports_memory_and_a_name() {
        let (cpu, memory) = SystemSensor::new().read_slow();
        assert!(cpu.name.is_some());
        let total = memory.total_bytes.expect("объём памяти известен");
        assert!(memory.used_bytes.unwrap() <= total);
    }
}

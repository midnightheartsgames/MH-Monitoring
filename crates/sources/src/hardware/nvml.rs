//! Видеокарта NVIDIA через NVML — ту же библиотеку, что читает оверлей NVIDIA.
//!
//! Одна карта выбирается при открытии и дальше читается для всех полей, иначе имя могло бы быть
//! от одной карты, а температура от другой.
//!
//! Видеопамять по процессу здесь не читается: под WDDM NVML всегда отвечает «недоступно» — память
//! распределяет ядро Windows, а не драйвер NVIDIA.

use mh_core::{GpuStats, SectionHealth, SensorReason};
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::error::NvmlError;

use super::gpu_counters::same_gpu_name;

pub struct NvmlSensor {
    nvml: Nvml,
    index: u32,
    name: Option<String>,
}

impl NvmlSensor {
    /// `wanted` — имя выбранной карты. Карты NVIDIA с таким именем нет — ошибка: выбранную,
    /// возможно, читает ядро графики.
    pub fn open(wanted: Option<&str>) -> Result<Self, SensorReason> {
        let nvml = Nvml::init().map_err(|error| init_reason(&error))?;
        let count = nvml.device_count().map_err(|_| SensorReason::GpuQueryFailed)?;

        let cards: Vec<(u32, String, u64)> = (0..count)
            .filter_map(|index| {
                let device = nvml.device_by_index(index).ok()?;
                let total = device.memory_info().ok()?.total;
                Some((index, device.name().unwrap_or_default(), total))
            })
            .collect();
        let chosen = match wanted {
            Some(wanted) => cards.iter().find(|(_, name, _)| same_gpu_name(name, wanted)),
            // Из нескольких карт — та, у которой больше памяти: в игровой машине это основная.
            None => cards.iter().max_by_key(|(_, _, total)| *total),
        };
        let (index, name, _) = chosen.cloned().ok_or(SensorReason::NoSupportedGpu)?;
        Ok(Self { nvml, index, name: Some(name).filter(|name| !name.is_empty()) })
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Частый тир: загрузка и частоты.
    pub fn read_load(&self) -> GpuStats {
        let device = match self.nvml.device_by_index(self.index) {
            Ok(device) => device,
            Err(_) => return lost(),
        };
        let mut health = FieldHealth::default();
        GpuStats {
            load_percent: health.take(device.utilization_rates()).map(|rates| f64::from(rates.gpu)),
            core_clock_mhz: health.take(device.clock_info(Clock::Graphics)).map(f64::from),
            memory_clock_mhz: health.take(device.clock_info(Clock::Memory)).map(f64::from),
            health: health.finish(),
            ..Default::default()
        }
    }

    /// Редкий тир: имя, температура, питание, видеопамять, вентилятор.
    pub fn read_slow(&self) -> GpuStats {
        let device = match self.nvml.device_by_index(self.index) {
            Ok(device) => device,
            Err(_) => return lost(),
        };
        let mut health = FieldHealth::default();
        let temperature_c = health.take(device.temperature(TemperatureSensor::Gpu)).map(f64::from);
        // NVML отдаёт милливатты.
        let power_watts = health.take(device.power_usage()).map(|mw| f64::from(mw) / 1_000.0);
        let memory = health.take(device.memory_info());
        let fan_rpm = health.take(device.fan_speed_rpm(0)).map(f64::from);
        let fan_percent = health.take(device.fan_speed(0)).map(f64::from);
        GpuStats {
            health: health.finish(),
            name: self.name.clone(),
            temperature_c,
            power_watts,
            vram_used_bytes: memory.as_ref().map(|memory| memory.used),
            vram_total_bytes: memory.map(|memory| memory.total),
            fan_rpm,
            fan_percent,
            ..Default::default()
        }
    }
}

/// Почему NVML не поднялась.
fn init_reason(error: &NvmlError) -> SensorReason {
    match error {
        // Нет nvml.dll или не запущен драйвер — для пользователя это одно: драйвера NVIDIA нет.
        NvmlError::LibloadingError(_)
        | NvmlError::LibraryNotFound
        | NvmlError::FunctionNotFound
        | NvmlError::DriverNotLoaded => SensorReason::GpuDriverMissing,
        _ => SensorReason::GpuQueryFailed,
    }
}

/// Карта не нашлась по своему индексу — отвалилась или драйвер перезапускается.
fn lost() -> GpuStats {
    GpuStats { health: SectionHealth::error(SensorReason::GpuQueryFailed), ..Default::default() }
}

/// Сводит здоровье по полям секции.
///
/// «Не поддерживается» — свойство карты, а не поломка: у ноутбучных карт нет тахометра, и секция
/// от этого не становится частичной. Настоящие ошибки делают секцию частичной, а потеря карты —
/// сломанной.
#[derive(Default)]
struct FieldHealth {
    read: usize,
    failed: usize,
    lost: bool,
}

impl FieldHealth {
    fn take<T>(&mut self, result: Result<T, NvmlError>) -> Option<T> {
        match result {
            Ok(value) => {
                self.read += 1;
                Some(value)
            }
            Err(NvmlError::NotSupported) => None,
            Err(NvmlError::GpuLost) => {
                self.lost = true;
                None
            }
            Err(_) => {
                self.failed += 1;
                None
            }
        }
    }

    fn finish(self) -> SectionHealth {
        if self.lost || (self.read == 0 && self.failed > 0) {
            SectionHealth::error(SensorReason::GpuQueryFailed)
        } else if self.failed > 0 {
            SectionHealth::partial(SensorReason::GpuQueryFailed)
        } else {
            SectionHealth::available()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mh_core::SensorStatus;

    #[test]
    fn a_missing_library_means_no_driver() {
        assert_eq!(init_reason(&NvmlError::LibraryNotFound), SensorReason::GpuDriverMissing);
        assert_eq!(init_reason(&NvmlError::DriverNotLoaded), SensorReason::GpuDriverMissing);
        assert_eq!(init_reason(&NvmlError::NoPermission), SensorReason::GpuQueryFailed);
    }

    #[test]
    fn unsupported_fields_do_not_degrade_the_section() {
        let mut health = FieldHealth::default();
        health.take(Ok(1));
        health.take::<u32>(Err(NvmlError::NotSupported));
        assert_eq!(health.finish(), SectionHealth::available());
    }

    #[test]
    fn some_failures_make_the_section_partial() {
        let mut health = FieldHealth::default();
        health.take(Ok(1));
        health.take::<u32>(Err(NvmlError::Unknown));
        assert_eq!(health.finish(), SectionHealth::partial(SensorReason::GpuQueryFailed));
    }

    #[test]
    fn a_lost_gpu_is_an_error_even_with_other_fields_read() {
        let mut health = FieldHealth::default();
        health.take(Ok(1));
        health.take::<u32>(Err(NvmlError::GpuLost));
        assert_eq!(health.finish().status, SensorStatus::Error);
    }

    /// На машине разработчика — NVIDIA: оверлей показывает загрузку, температуру, частоты,
    /// видеопамять и питание, и мы обязаны показать то же. На машине без NVIDIA — внятная причина.
    #[test]
    fn this_machine_either_reads_or_explains() {
        match NvmlSensor::open(None) {
            Ok(sensor) => {
                let load = sensor.read_load();
                let slow = sensor.read_slow();
                assert!(sensor.name().is_some());
                assert!(load.load_percent.is_some());
                assert!(load.core_clock_mhz.is_some());
                assert!(slow.temperature_c.is_some());
                assert!(slow.vram_total_bytes.is_some());
                assert!(slow.power_watts.is_some());
            }
            Err(reason) => assert!(!reason.message().is_empty()),
        }
    }
}

//! Сборщик показаний железа по тирам опроса.
//!
//! Каждый датчик открывается один раз. Недоступный датчик не ломает остальные — он превращается в
//! причину, которую видно в секции.

use mh_core::{
    GpuStats, HardwareSample, Millis, SampleTier, SectionHealth, SensorReason, SensorStatus,
};
use mh_platform::pawnio::PawnIoError;

use super::amd_sensor::{AmdCpuSensor, AmdSensorError};
use super::cpuid::identify;
use super::nvml::NvmlSensor;
use super::system::SystemSensor;

pub struct HardwareSampler {
    system: SystemSensor,
    gpu: Result<NvmlSensor, SensorReason>,
    cpu_thermals: Result<AmdCpuSensor, SensorReason>,
}

impl Default for HardwareSampler {
    fn default() -> Self {
        Self::open()
    }
}

impl HardwareSampler {
    pub fn open() -> Self {
        Self {
            system: SystemSensor::new(),
            gpu: NvmlSensor::open(),
            cpu_thermals: AmdCpuSensor::open(&identify()).map_err(|error| cpu_reason(&error)),
        }
    }

    /// Почему не читаются температура и мощность CPU, если не читаются.
    pub fn cpu_thermals_reason(&self) -> Option<SensorReason> {
        self.cpu_thermals.as_ref().err().copied()
    }

    pub fn sample(&mut self, tier: SampleTier, now_ms: Millis) -> HardwareSample {
        let (gpu, cpu, memory) = match tier {
            SampleTier::Load => {
                let gpu = match &self.gpu {
                    Ok(sensor) => sensor.read_load(),
                    Err(reason) => unavailable_gpu(*reason),
                };
                (gpu, self.system.read_load(), Default::default())
            }
            SampleTier::Slow => {
                let gpu = match &self.gpu {
                    Ok(sensor) => sensor.read_slow(),
                    Err(reason) => unavailable_gpu(*reason),
                };
                let (mut cpu, memory) = self.system.read_slow();
                match &mut self.cpu_thermals {
                    Ok(sensor) => {
                        let reading = sensor.read(now_ms);
                        cpu.temperature_c = reading.temperature_c;
                        cpu.power_watts = reading.power_watts;
                        // Мощности нет на первом замере — ей нужен второй. Отсутствие только её
                        // поломкой не считается.
                        cpu.health = if reading.temperature_c.is_some() {
                            SectionHealth::available()
                        } else {
                            SectionHealth::partial(SensorReason::SensorReadFailed)
                        };
                    }
                    Err(reason) => cpu.health = SectionHealth::partial(*reason),
                }
                (gpu, cpu, memory)
            }
        };
        let status = overall_status(&[gpu.health, cpu.health, memory.health]);
        HardwareSample { gpu, cpu, memory, status }
    }
}

fn unavailable_gpu(reason: SensorReason) -> GpuStats {
    GpuStats { health: SectionHealth::unavailable(reason), ..Default::default() }
}

fn cpu_reason(error: &AmdSensorError) -> SensorReason {
    match error {
        AmdSensorError::Unsupported => SensorReason::CpuNotSupported,
        AmdSensorError::PawnIo(PawnIoError::NotInstalled) => SensorReason::PawnIoMissing,
        AmdSensorError::PawnIo(PawnIoError::AccessDenied) => SensorReason::NeedsAdmin,
        AmdSensorError::PawnIo(_) => SensorReason::SensorReadFailed,
    }
}

/// Итог по железу для [`HardwareSample::status`].
///
/// Не худшее из секций: машина без NVIDIA — нормальная машина, и CPU с памятью у неё показываются.
/// Поэтому «всё исправно» только когда исправно всё, «частично», если хоть что-то читается, и
/// худшее — когда не читается ничего. Секции, о которых тир молчит, не учитываются.
fn overall_status(sections: &[SectionHealth]) -> SensorStatus {
    let known: Vec<SensorStatus> = sections
        .iter()
        .map(|health| health.status)
        .filter(|status| *status != SensorStatus::Unknown)
        .collect();
    if known.is_empty() {
        SensorStatus::Unknown
    } else if known.iter().all(|status| *status == SensorStatus::Available) {
        SensorStatus::Available
    } else if known.iter().any(|status| status.is_usable()) {
        SensorStatus::Partial
    } else {
        known.into_iter().fold(SensorStatus::Available, SensorStatus::worst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pawnio_errors_map_to_reasons_the_user_can_act_on() {
        assert_eq!(cpu_reason(&AmdSensorError::Unsupported), SensorReason::CpuNotSupported);
        assert_eq!(
            cpu_reason(&AmdSensorError::PawnIo(PawnIoError::NotInstalled)),
            SensorReason::PawnIoMissing
        );
        assert_eq!(
            cpu_reason(&AmdSensorError::PawnIo(PawnIoError::AccessDenied)),
            SensorReason::NeedsAdmin
        );
    }

    #[test]
    fn a_machine_without_nvidia_is_partial_not_unavailable() {
        let status = overall_status(&[
            SectionHealth::unavailable(SensorReason::GpuDriverMissing),
            SectionHealth::available(),
            SectionHealth::available(),
        ]);
        assert_eq!(status, SensorStatus::Partial);
    }

    #[test]
    fn nothing_readable_reports_the_worst() {
        let status = overall_status(&[
            SectionHealth::unavailable(SensorReason::GpuDriverMissing),
            SectionHealth::error(SensorReason::SensorReadFailed),
        ]);
        assert_eq!(status, SensorStatus::Error);
    }

    #[test]
    fn silent_sections_are_ignored() {
        assert_eq!(
            overall_status(&[SectionHealth::available(), SectionHealth::default()]),
            SensorStatus::Available
        );
        assert_eq!(overall_status(&[SectionHealth::default()]), SensorStatus::Unknown);
    }
}

//! Температура и мощность CPU AMD Zen через PawnIO.

use mh_core::Millis;
use mh_platform::pawnio::{PawnIo, PawnIoError, PciAccessLock};

use super::amd::{
    EnergyMeter, MSR_AMD_PKG_ENERGY_STATUS, MSR_AMD_RAPL_POWER_UNIT, ZEN_REPORTED_TEMP_CTRL,
    decode_tctl, energy_unit_joules, tctl_offset,
};
use super::cpuid::CpuIdentity;

/// Подписанный модуль PawnIO для AMD Zen. Встроен в исполняемый файл: по плану приложение —
/// один файл (PLAN.md §3). Происхождение, хеш и лицензия — `assets/pawnio/NOTICE.md`.
const AMD_FAMILY17_MODULE: &[u8] = include_bytes!("../../../../assets/pawnio/AMDFamily17.bin");

/// Сколько ждать общий мьютекс PCI. Держат его на время одного чтения, поэтому дольше ждать
/// бессмысленно: если занят так долго, значит кто-то завис, и лучше пропустить показание.
const PCI_LOCK_TIMEOUT_MS: u32 = 50;

/// Почему датчик недоступен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmdSensorError {
    /// Не AMD Zen: модуль рассчитан на семейства 17h–1Ah.
    Unsupported,
    PawnIo(PawnIoError),
}

impl std::fmt::Display for AmdSensorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AmdSensorError::Unsupported => write!(formatter, "процессор не AMD Zen"),
            AmdSensorError::PawnIo(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for AmdSensorError {}

/// Одно показание. Каждое поле может отсутствовать само по себе.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CpuThermals {
    pub temperature_c: Option<f64>,
    pub power_watts: Option<f64>,
}

pub struct AmdCpuSensor {
    pawnio: PawnIo,
    pci_lock: PciAccessLock,
    tctl_offset: f64,
    meter: Option<EnergyMeter>,
}

impl AmdCpuSensor {
    pub fn open(cpu: &CpuIdentity) -> Result<Self, AmdSensorError> {
        if !cpu.is_amd_zen() {
            return Err(AmdSensorError::Unsupported);
        }
        let pawnio =
            PawnIo::open_with_module(AMD_FAMILY17_MODULE).map_err(AmdSensorError::PawnIo)?;
        let pci_lock = PciAccessLock::open().map_err(AmdSensorError::PawnIo)?;

        let mut sensor = Self {
            pawnio,
            pci_lock,
            tctl_offset: tctl_offset(cpu.family, &cpu.brand),
            meter: None,
        };
        // Единица энергии постоянна — читаем её один раз. Если не вышло, мощности просто не будет,
        // а температура от этого не зависит.
        sensor.meter = sensor
            .read_msr(MSR_AMD_RAPL_POWER_UNIT)
            .ok()
            .map(|register| EnergyMeter::new(energy_unit_joules(register)));
        Ok(sensor)
    }

    pub fn read(&mut self, now_ms: Millis) -> CpuThermals {
        let temperature_c = self.read_tctl().map(|tctl| tctl - self.tctl_offset);
        let power_watts = match self.read_msr(MSR_AMD_PKG_ENERGY_STATUS) {
            Ok(counter) => {
                self.meter.as_mut().and_then(|meter| meter.update(counter as u32, now_ms))
            }
            Err(_) => None,
        };
        CpuThermals { temperature_c, power_watts }
    }

    fn read_msr(&self, msr: u64) -> Result<u64, PawnIoError> {
        let mut out = [0u64; 1];
        self.pawnio.execute("ioctl_read_msr", &[msr], &mut out)?;
        Ok(out[0])
    }

    /// `None` — в этот раз значения нет: мьютекс занят другим монитором или драйвер отказал.
    /// Для показания это одно и то же, следующее придёт со следующим опросом тира.
    fn read_tctl(&self) -> Option<f64> {
        // Пара «индекс/данные» PCI общая для всей системы — читать только под мьютексом.
        let register = self.pci_lock.with(PCI_LOCK_TIMEOUT_MS, || {
            let mut out = [0u64; 1];
            self.pawnio
                .execute("ioctl_read_smn", &[ZEN_REPORTED_TEMP_CTRL], &mut out)
                .map(|_| out[0])
        })?;
        register.ok().map(|register| decode_tctl(register as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::cpuid::{Vendor, identify};

    #[test]
    fn the_bundled_module_is_the_audited_one() {
        // Размер и хеш зафиксированы в assets/pawnio/NOTICE.md. Размер проверяется здесь, чтобы
        // случайная замена файла не прошла незамеченной.
        assert_eq!(AMD_FAMILY17_MODULE.len(), 10_652);
    }

    #[test]
    fn a_non_zen_cpu_is_rejected_before_touching_the_driver() {
        let intel =
            CpuIdentity { vendor: Vendor::Intel, family: 6, model: 0x9E, brand: "Intel".into() };
        assert_eq!(AmdCpuSensor::open(&intel).err(), Some(AmdSensorError::Unsupported));
    }

    /// На этой машине — Ryzen. С правами и установленным PawnIO датчик обязан дать температуру
    /// в разумных пределах; без прав — внятный отказ.
    #[test]
    fn this_machine_either_reads_or_explains() {
        let cpu = identify();
        if !cpu.is_amd_zen() {
            return;
        }
        match AmdCpuSensor::open(&cpu) {
            Ok(mut sensor) => {
                let reading = sensor.read(0);
                let celsius = reading.temperature_c.expect("температура читается");
                assert!((5.0..=110.0).contains(&celsius), "Tctl = {celsius}");
            }
            Err(error) => assert!(!error.to_string().is_empty()),
        }
    }
}

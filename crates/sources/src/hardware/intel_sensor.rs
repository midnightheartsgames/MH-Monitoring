//! Температура и мощность CPU Intel через PawnIO.
//!
//! Проверено только тестами расшифровки: на машине разработчика процессор AMD.

use mh_core::Millis;
use mh_platform::pawnio::{PawnIo, PawnIoError};

use super::amd::{EnergyMeter, energy_unit_joules};
use super::amd_sensor::{CpuSensorError, CpuThermals};
use super::cpuid::{CpuIdentity, Vendor};
use super::intel::{
    MSR_IA32_PACKAGE_THERM_STATUS, MSR_IA32_TEMPERATURE_TARGET, MSR_IA32_THERM_STATUS,
    MSR_PKG_ENERGY_STATUS, MSR_RAPL_POWER_UNIT, core_temperature, has_digital_sensor,
    has_package_sensor, package_temperature, tj_max,
};

/// Подписанный модуль PawnIO для MSR Intel. Происхождение, хеш и лицензия —
/// `assets/pawnio/NOTICE.md`.
const INTEL_MSR_MODULE: &[u8] = include_bytes!("../../../../assets/pawnio/IntelMSR.bin");

pub struct IntelCpuSensor {
    pawnio: PawnIo,
    tj_max: f64,
    package_sensor: bool,
    meter: Option<EnergyMeter>,
}

impl IntelCpuSensor {
    /// `leaf6_eax` — CPUID.06H:EAX: есть ли цифровой датчик и датчик пакета.
    pub fn open(cpu: &CpuIdentity, leaf6_eax: u32) -> Result<Self, CpuSensorError> {
        if cpu.vendor != Vendor::Intel || !has_digital_sensor(leaf6_eax) {
            return Err(CpuSensorError::Unsupported);
        }
        let pawnio = PawnIo::open_with_module(INTEL_MSR_MODULE).map_err(CpuSensorError::PawnIo)?;
        let mut sensor = Self {
            pawnio,
            tj_max: 0.0,
            package_sensor: has_package_sensor(leaf6_eax),
            meter: None,
        };
        sensor.tj_max = tj_max(sensor.read_msr(MSR_IA32_TEMPERATURE_TARGET).unwrap_or(0));
        // Формула RAPL у Intel та же, что у AMD (`rapl.c`); без единицы нет только мощности.
        sensor.meter = sensor
            .read_msr(MSR_RAPL_POWER_UNIT)
            .ok()
            .map(|register| EnergyMeter::new(energy_unit_joules(register)));
        Ok(sensor)
    }

    pub fn read(&mut self, now_ms: Millis) -> CpuThermals {
        let temperature_c = if self.package_sensor {
            self.read_msr(MSR_IA32_PACKAGE_THERM_STATUS)
                .ok()
                .map(|status| package_temperature(status, self.tj_max))
        } else {
            // Старые процессоры без датчика пакета: ядро, на котором выполнилось чтение.
            self.read_msr(MSR_IA32_THERM_STATUS)
                .ok()
                .and_then(|status| core_temperature(status, self.tj_max))
        };
        let power_watts = match self.read_msr(MSR_PKG_ENERGY_STATUS) {
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
}

/// CPUID.06H:EAX этой машины.
#[cfg(target_arch = "x86_64")]
pub fn thermal_leaf() -> u32 {
    use std::arch::x86_64::__cpuid;
    if __cpuid(0).eax >= 6 { __cpuid(6).eax } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_module_is_the_audited_one() {
        // Размер и хеш зафиксированы в assets/pawnio/NOTICE.md.
        assert_eq!(INTEL_MSR_MODULE.len(), 5_324);
    }

    #[test]
    fn a_non_intel_cpu_is_rejected_before_touching_the_driver() {
        let amd =
            CpuIdentity { vendor: Vendor::Amd, family: 0x19, model: 0x21, brand: "AMD".into() };
        assert_eq!(IntelCpuSensor::open(&amd, 0xF7).err(), Some(CpuSensorError::Unsupported));
    }

    #[test]
    fn an_intel_cpu_without_a_digital_sensor_is_rejected() {
        let old = CpuIdentity { vendor: Vendor::Intel, family: 6, model: 0x0F, brand: "".into() };
        assert_eq!(IntelCpuSensor::open(&old, 0).err(), Some(CpuSensorError::Unsupported));
    }
}

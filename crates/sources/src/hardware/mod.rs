//! Источники показаний железа (PLAN.md §6/P3).
//!
//! Чистые части — расшифровка регистров и опознание процессора — собираются везде. Чтение через
//! драйвер живёт только под Windows.

pub mod amd;
pub mod cpuid;
pub mod gpu_counters;
pub mod intel;
pub mod nvml;
pub mod system;

#[cfg(windows)]
pub mod amd_sensor;
#[cfg(windows)]
pub mod intel_sensor;
#[cfg(windows)]
pub mod sampler;
#[cfg(windows)]
pub mod wddm;

//! Что и как часто опрашивать — настройка пользователя, которую UI передаёт движку.
//!
//! Частота кадров сюда не входит: FPS считается по окну кадров и опрашивается с постоянной
//! частотой, иначе «текущий FPS» менял бы смысл вместе с настройкой.

use crate::Millis;

/// Интервалы, из которых выбирает пользователь.
pub const HARDWARE_INTERVALS_MS: [Millis; 4] = [250, 500, 1_000, 2_000];
/// Интервал по умолчанию — тот, что был зашит до появления настройки.
pub const DEFAULT_HARDWARE_INTERVAL_MS: Millis = 500;
/// Редкий тир (температуры, мощность, память) чаще раза в секунду не опрашивается: эти значения
/// так быстро не меняются, а чтение через PawnIO и NVML не бесплатно.
pub const MIN_SLOW_INTERVAL_MS: Millis = 1_000;
/// Самое длинное имя видеокарты, которое принимается из файла или канала.
const MAX_GPU_NAME_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct SensorOptions {
    /// Как часто обновляются загрузка и частоты.
    pub hardware_interval_ms: Millis,
    /// Какую видеокарту читать — по имени, как его сообщает система. `None` — выбрать самому:
    /// карту с наибольшей памятью.
    ///
    /// Имя, а не номер или LUID: номер меняется при подключении второй карты, LUID — при
    /// каждой загрузке. Две одинаковые карты по имени не различить — тогда берётся первая.
    pub gpu: Option<String>,
}

/// Интервалы опроса тиров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intervals {
    pub load_ms: Millis,
    pub slow_ms: Millis,
}

impl Default for SensorOptions {
    fn default() -> Self {
        Self { hardware_interval_ms: DEFAULT_HARDWARE_INTERVAL_MS, gpu: None }
    }
}

impl SensorOptions {
    /// Ближайший допустимый интервал: чужое число из файла или канала не разгоняет опрос.
    /// Пустое имя видеокарты — автовыбор.
    pub fn sanitized(self) -> SensorOptions {
        let wanted = self.hardware_interval_ms;
        let hardware_interval_ms = HARDWARE_INTERVALS_MS
            .into_iter()
            .min_by_key(|interval| interval.abs_diff(wanted))
            .unwrap_or(DEFAULT_HARDWARE_INTERVAL_MS);
        let gpu = self
            .gpu
            .map(|name| name.trim().chars().take(MAX_GPU_NAME_CHARS).collect::<String>())
            .filter(|name| !name.is_empty());
        SensorOptions { hardware_interval_ms, gpu }
    }

    /// Частый тир — выбранный интервал; редкий — не чаще раза в секунду и не реже частого.
    pub fn interval(&self) -> Intervals {
        let load_ms = self.clone().sanitized().hardware_interval_ms;
        Intervals { load_ms, slow_ms: load_ms.max(MIN_SLOW_INTERVAL_MS) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every(ms: Millis) -> SensorOptions {
        SensorOptions { hardware_interval_ms: ms, gpu: None }
    }

    #[test]
    fn any_number_becomes_the_nearest_allowed_interval() {
        assert_eq!(every(0).sanitized().hardware_interval_ms, 250);
        assert_eq!(every(700).sanitized().hardware_interval_ms, 500);
        assert_eq!(every(900).sanitized().hardware_interval_ms, 1_000);
        assert_eq!(every(u64::MAX).sanitized().hardware_interval_ms, 2_000);
    }

    #[test]
    fn the_slow_tier_never_runs_faster_than_once_a_second() {
        assert_eq!(every(250).interval(), Intervals { load_ms: 250, slow_ms: 1_000 });
        assert_eq!(every(2_000).interval(), Intervals { load_ms: 2_000, slow_ms: 2_000 });
        assert_eq!(SensorOptions::default().interval().load_ms, 500);
    }

    #[test]
    fn a_blank_gpu_name_means_automatic() {
        let blank = SensorOptions { gpu: Some("   ".into()), ..Default::default() };
        assert_eq!(blank.sanitized().gpu, None);
        let named = SensorOptions { gpu: Some(" RTX 5070 Ti ".into()), ..Default::default() };
        assert_eq!(named.sanitized().gpu.as_deref(), Some("RTX 5070 Ti"));
    }
}

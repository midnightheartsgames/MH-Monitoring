//! Статистика кадров по окну frametime.
//!
//! «1 % low» — это среднее по самому медленному проценту кадров, выраженное в FPS. Считается
//! оно **по frametime**, и только результат переводится в FPS: если усреднять сами значения FPS,
//! быстрые кадры получают несоразмерный вес и цифра перестаёт описывать просадки.
//!
//! Перенесено по смыслу из `FpsStatisticsCalculator.kt` старого проекта (PLAN.md §7.1) вместе с
//! минимумами выборки — они там выбраны не произвольно.

/// Ниже этого числа кадров «худший процент» описывает шум, а не плавность: из пятидесяти кадров
/// один процент — это один кадр.
pub const MIN_SAMPLES_1_PERCENT: usize = 100;
/// То же для 0.1 %.
pub const MIN_SAMPLES_0_1_PERCENT: usize = 1_000;

/// Показатели плавности за окно.
///
/// Всё, чего может не быть, — `None`, а не ноль: отсутствующая цифра и цифра «ноль» означают
/// разное, и UI обязан их различать.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameStatistics {
    /// FPS за последние [`CURRENT_WINDOW_MS`] — число для HUD. Покадровую картину даёт график.
    pub current_fps: Option<f64>,
    pub average_fps: Option<f64>,
    pub low_1_percent_fps: Option<f64>,
    pub low_0_1_percent_fps: Option<f64>,
    pub current_frametime_ms: Option<f64>,
    /// Сколько значений реально попало в расчёт — после отбраковки негодных.
    pub sample_count: usize,
    /// Сколько значений отброшено как негодные. Ненулевое — повод посмотреть на источник.
    pub rejected_count: usize,
}

impl FrameStatistics {
    pub const EMPTY: Self = Self {
        current_fps: None,
        average_fps: None,
        low_1_percent_fps: None,
        low_0_1_percent_fps: None,
        current_frametime_ms: None,
        sample_count: 0,
        rejected_count: 0,
    };

    /// Есть ли что показывать. Отличает «идёт замер» от «окно пустое».
    pub fn has_samples(&self) -> bool {
        self.sample_count > 0
    }
}

/// Считает статистику, выделяя временный буфер под сортировку.
///
/// В горячем пути лучше [`compute_with_scratch`]: при 4 Гц обновления и окне на 16384 кадра
/// это 64 КБ аллокации на каждый опрос впустую.
pub fn compute(frametimes_ms: &[f32]) -> FrameStatistics {
    let mut scratch = Vec::new();
    compute_with_scratch(frametimes_ms, &mut scratch)
}

/// То же, но с переиспользуемым буфером: `scratch` очищается и заполняется заново.
pub fn compute_with_scratch(frametimes_ms: &[f32], scratch: &mut Vec<f32>) -> FrameStatistics {
    scratch.clear();
    scratch.reserve(frametimes_ms.len());

    let mut rejected = 0usize;
    for &value in frametimes_ms {
        // NaN отравляет сортировку, ноль и отрицательные дают деление на ноль дальше по цепочке.
        // Отбраковываем на входе, а не посреди расчёта.
        if value.is_finite() && value > 0.0 {
            scratch.push(value);
        } else {
            rejected += 1;
        }
    }

    if scratch.is_empty() {
        return FrameStatistics { rejected_count: rejected, ..FrameStatistics::EMPTY };
    }

    // До сортировки: «текущее» — это самые свежие кадры, а порядок нужен хронологический.
    let current_frametime = recent_average_ms(scratch);

    let count = scratch.len();
    let total_ms: f64 = scratch.iter().map(|&v| v as f64).sum();
    let average_fps = if total_ms > 0.0 { Some(count as f64 * 1000.0 / total_ms) } else { None };

    // Все значения уже проверены на конечность, поэтому сравнение полное и `unwrap` не нужен.
    scratch.sort_by(|a, b| a.partial_cmp(b).expect("NaN отбракован выше"));

    FrameStatistics {
        current_fps: Some(1000.0 / current_frametime),
        average_fps,
        low_1_percent_fps: low_fps(scratch, 0.01, MIN_SAMPLES_1_PERCENT),
        low_0_1_percent_fps: low_fps(scratch, 0.001, MIN_SAMPLES_0_1_PERCENT),
        current_frametime_ms: Some(current_frametime),
        sample_count: count,
        rejected_count: rejected,
    }
}

/// Окно «текущего» значения. По одному последнему кадру число в HUD скакало у Dota 2 от 63 до
/// 390 за секунду (PLAN.md §2.13); покадровую картину показывает график.
pub const CURRENT_WINDOW_MS: f64 = 500.0;

/// Средний frametime последних кадров, покрывающих [`CURRENT_WINDOW_MS`] (хотя бы одного).
/// `chronological` не пуст и содержит только годные значения.
fn recent_average_ms(chronological: &[f32]) -> f64 {
    let mut total = 0.0f64;
    let mut count = 0usize;
    for &value in chronological.iter().rev() {
        total += f64::from(value);
        count += 1;
        if total >= CURRENT_WINDOW_MS {
            break;
        }
    }
    total / count as f64
}

/// `sorted` — по возрастанию, поэтому самые медленные кадры лежат в хвосте.
fn low_fps(sorted: &[f32], fraction: f64, min_samples: usize) -> Option<f64> {
    let count = sorted.len();
    if count < min_samples {
        return None;
    }
    let slow_count = ((count as f64) * fraction).ceil() as usize;
    let slow_count = slow_count.clamp(1, count);
    let sum: f64 = sorted[count - slow_count..].iter().map(|&v| v as f64).sum();
    let average_ms = sum / slow_count as f64;
    if average_ms > 0.0 { Some(1000.0 / average_ms) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(count: usize, ms: f32) -> Vec<f32> {
        vec![ms; count]
    }

    #[test]
    fn average_is_frames_over_total_time() {
        let stats = compute(&constant(120, 16.0));
        assert_eq!(stats.sample_count, 120);
        assert!((stats.average_fps.unwrap() - 62.5).abs() < 1e-9);
    }

    #[test]
    fn current_values_come_from_the_last_half_second() {
        // Секунда по 10 мс, потом полсекунды по 20 мс: «текущее» — только последние полсекунды.
        let mut frames = vec![10.0; 100];
        frames.extend(vec![20.0; 25]);
        let stats = compute(&frames);
        assert_eq!(stats.current_frametime_ms, Some(20.0));
        assert!((stats.current_fps.unwrap() - 50.0).abs() < 1e-9);
    }

    /// Живой случай Dota 2: чередование быстрых и медленных кадров не должно дёргать число.
    #[test]
    fn alternating_frames_give_a_steady_current_value() {
        let frames: Vec<f32> = (0..200).map(|i| if i % 2 == 0 { 3.0 } else { 13.0 }).collect();
        let current = compute(&frames).current_fps.unwrap();
        assert!((current - 125.0).abs() < 2.0, "{current}");
    }

    #[test]
    fn a_short_history_uses_what_there_is() {
        let stats = compute(&[10.0, 30.0]);
        assert_eq!(stats.current_frametime_ms, Some(20.0));
    }

    #[test]
    fn invalid_samples_are_rejected_not_propagated() {
        let stats = compute(&[16.0, f32::NAN, -1.0, 0.0, f32::INFINITY, 16.0]);
        assert_eq!(stats.sample_count, 2);
        assert_eq!(stats.rejected_count, 4);
        assert!(stats.average_fps.unwrap().is_finite());
    }

    #[test]
    fn an_empty_window_yields_no_statistics() {
        assert!(!compute(&[]).has_samples());
        assert!(!compute(&[f32::NAN, 0.0]).has_samples());
        assert_eq!(compute(&[f32::NAN, 0.0]).rejected_count, 2);
    }

    // --- границы выборки: 99/100/101 и 999/1000/1001 (PLAN.md §6/P1) ---

    #[test]
    fn one_percent_low_needs_one_hundred_samples() {
        assert!(compute(&constant(99, 16.0)).low_1_percent_fps.is_none());
        assert!(compute(&constant(100, 16.0)).low_1_percent_fps.is_some());
        assert!(compute(&constant(101, 16.0)).low_1_percent_fps.is_some());
    }

    #[test]
    fn zero_point_one_percent_low_needs_one_thousand_samples() {
        assert!(compute(&constant(999, 16.0)).low_0_1_percent_fps.is_none());
        assert!(compute(&constant(1000, 16.0)).low_0_1_percent_fps.is_some());
        assert!(compute(&constant(1001, 16.0)).low_0_1_percent_fps.is_some());
    }

    #[test]
    fn the_threshold_counts_valid_samples_not_received_ones() {
        // 100 пришло, одно негодное — значит выборка 99, и процента быть не должно.
        let mut frames = constant(99, 16.0);
        frames.push(f32::NAN);
        let stats = compute(&frames);
        assert_eq!(stats.sample_count, 99);
        assert!(stats.low_1_percent_fps.is_none());
    }

    #[test]
    fn one_percent_low_averages_the_slowest_frames() {
        // 99 кадров по 10 мс и один 100 мс: ceil(100 × 0.01) = 1, то есть ровно худший кадр.
        let mut frames = constant(99, 10.0);
        frames.push(100.0);
        let stats = compute(&frames);
        assert!((stats.low_1_percent_fps.unwrap() - 10.0).abs() < 1e-6);
    }

    #[test]
    fn lows_come_from_frametimes_not_from_averaged_fps() {
        // 990 кадров по 5 мс (200 FPS) и 10 кадров по 100 мс (10 FPS).
        // ceil(1000 × 0.01) = 10 → 1 % low обязан быть ровно 10 FPS.
        // Усреднение FPS дало бы (990×200 + 10×10) / 1000 ≈ 198 — цифра, ничего не говорящая
        // о просадках.
        let mut frames = constant(990, 5.0);
        frames.extend(constant(10, 100.0));
        let stats = compute(&frames);
        assert!((stats.low_1_percent_fps.unwrap() - 10.0).abs() < 1e-6);
    }

    #[test]
    fn a_single_spike_does_not_dominate_the_average() {
        let mut frames = constant(999, 10.0);
        frames.push(500.0);
        let stats = compute(&frames);
        // 1000 кадров за 10.49 с — около 95 FPS, а не 2.
        let avg = stats.average_fps.unwrap();
        assert!(avg > 90.0 && avg < 96.0, "average_fps = {avg}");
    }

    #[test]
    fn the_scratch_buffer_is_reusable() {
        let mut scratch = Vec::new();
        let first = compute_with_scratch(&constant(200, 8.0), &mut scratch);
        let second = compute_with_scratch(&constant(200, 8.0), &mut scratch);
        assert_eq!(first, second);
        // И на другом размере входа буфер не тащит хвост прошлого расчёта.
        let third = compute_with_scratch(&constant(50, 8.0), &mut scratch);
        assert_eq!(third.sample_count, 50);
    }
}

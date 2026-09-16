//! Статистика по frametime.
//!
//! Формулы взяты такими же, какими они будут в крейте `core` (PLAN.md §6/P1, §7.1), чтобы
//! числа спайка можно было сравнивать с числами приложения и с вендорским оверлеем.

/// Минимум выборки для 1 % low. Меньше — значение не имеет смысла: «худший процент» из
/// пятидесяти кадров это один кадр.
pub const MIN_SAMPLES_1_PERCENT: usize = 100;
/// Минимум выборки для 0.1 % low.
pub const MIN_SAMPLES_0_1_PERCENT: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStats {
    pub accepted: usize,
    pub rejected: usize,
    pub avg_fps: f64,
    pub avg_frametime_ms: f64,
    pub min_frametime_ms: f64,
    pub max_frametime_ms: f64,
    pub low_1_percent_fps: Option<f64>,
    pub low_0_1_percent_fps: Option<f64>,
}

/// Считает статистику по массиву frametime в миллисекундах.
///
/// Возвращает `None`, если не осталось ни одного валидного значения.
pub fn compute(frametimes: &[f32]) -> Option<FrameStats> {
    let mut valid: Vec<f64> = Vec::with_capacity(frametimes.len());
    let mut rejected = 0usize;
    for &value in frametimes {
        let value = value as f64;
        // Ноль и отрицательные значения дают деление на ноль дальше по цепочке, NaN
        // отравляет сортировку — оба случая отбрасываем на входе, а не в середине расчёта.
        if value.is_finite() && value > 0.0 {
            valid.push(value);
        } else {
            rejected += 1;
        }
    }
    if valid.is_empty() {
        return None;
    }

    let n = valid.len();
    let sum: f64 = valid.iter().sum();
    let avg_fps = n as f64 * 1000.0 / sum;
    let avg_frametime_ms = sum / n as f64;

    let mut sorted = valid.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min_frametime_ms = sorted[0];
    let max_frametime_ms = sorted[n - 1];

    Some(FrameStats {
        accepted: n,
        rejected,
        avg_fps,
        avg_frametime_ms,
        min_frametime_ms,
        max_frametime_ms,
        low_1_percent_fps: low_percentile(&sorted, 0.01, MIN_SAMPLES_1_PERCENT),
        low_0_1_percent_fps: low_percentile(&sorted, 0.001, MIN_SAMPLES_0_1_PERCENT),
    })
}

/// «Low» — не перцентиль, а среднее по самым медленным кадрам, переведённое в FPS.
/// Берётся `ceil(N * fraction)` худших значений.
fn low_percentile(sorted_ascending: &[f64], fraction: f64, min_samples: usize) -> Option<f64> {
    let n = sorted_ascending.len();
    if n < min_samples {
        return None;
    }
    let take = ((n as f64) * fraction).ceil() as usize;
    let take = take.max(1).min(n);
    let slowest = &sorted_ascending[n - take..];
    let mean: f64 = slowest.iter().sum::<f64>() / take as f64;
    if mean > 0.0 { Some(1000.0 / mean) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(count: usize, ms: f32) -> Vec<f32> {
        vec![ms; count]
    }

    #[test]
    fn average_is_frames_over_total_time() {
        let stats = compute(&constant(120, 16.0)).unwrap();
        assert!((stats.avg_fps - 62.5).abs() < 1e-9);
        assert!((stats.avg_frametime_ms - 16.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_samples_are_rejected_not_propagated() {
        let stats = compute(&[16.0, f32::NAN, -1.0, 0.0, f32::INFINITY, 16.0]).unwrap();
        assert_eq!(stats.accepted, 2);
        assert_eq!(stats.rejected, 4);
        assert!(stats.avg_fps.is_finite());
    }

    #[test]
    fn empty_input_has_no_statistics() {
        assert!(compute(&[]).is_none());
        assert!(compute(&[f32::NAN, 0.0]).is_none());
    }

    #[test]
    fn one_percent_low_needs_one_hundred_samples() {
        assert!(compute(&constant(99, 16.0)).unwrap().low_1_percent_fps.is_none());
        assert!(compute(&constant(100, 16.0)).unwrap().low_1_percent_fps.is_some());
        assert!(compute(&constant(101, 16.0)).unwrap().low_1_percent_fps.is_some());
    }

    #[test]
    fn zero_point_one_percent_low_needs_one_thousand_samples() {
        assert!(compute(&constant(999, 16.0)).unwrap().low_0_1_percent_fps.is_none());
        assert!(compute(&constant(1000, 16.0)).unwrap().low_0_1_percent_fps.is_some());
        assert!(compute(&constant(1001, 16.0)).unwrap().low_0_1_percent_fps.is_some());
    }

    #[test]
    fn one_percent_low_averages_the_slowest_frames() {
        // 100 кадров: 99 по 10 мс и один 100 мс. ceil(100 * 0.01) = 1 → ровно худший кадр.
        let mut frames = constant(99, 10.0);
        frames.push(100.0);
        let stats = compute(&frames).unwrap();
        assert!((stats.low_1_percent_fps.unwrap() - 10.0).abs() < 1e-6);
    }

    #[test]
    fn a_single_spike_does_not_dominate_the_average() {
        let mut frames = constant(999, 10.0);
        frames.push(500.0);
        let stats = compute(&frames).unwrap();
        // 1000 кадров за 10.49 с → около 95 FPS, а не 2.
        assert!(stats.avg_fps > 90.0 && stats.avg_fps < 96.0, "avg_fps = {}", stats.avg_fps);
        assert_eq!(stats.max_frametime_ms, 500.0);
        assert_eq!(stats.min_frametime_ms, 10.0);
    }
}

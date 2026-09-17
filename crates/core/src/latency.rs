//! Задержка вывода кадра: от начала работы CPU над кадром до его появления на экране.
//!
//! Так её определяет PresentMon («Display Latency»). В CSV 2.x отдельной колонки нет, она
//! складывается из трёх: `TimeInMs` (начало Present) − `CPUStartTimeInMs` (начало кадра) +
//! `MsUntilDisplayed` (от Present до экрана). У кадра, который на экран не попал, последней нет —
//! такой кадр в задержку не входит.
//!
//! HUD показывает среднее за последнюю секунду: покадровая задержка скачет, и живая цифра
//! нечитаема.

use std::collections::VecDeque;

use crate::Millis;

/// За сколько последних миллисекунд усредняется задержка.
pub const LATENCY_WINDOW_MS: Millis = 1_000;
/// Больше этого — не задержка, а сбой меток времени.
const MAX_LATENCY_MS: f64 = 1_000.0;
/// Сколько кадров хранить: при 1000 FPS окно в секунду всё равно не переполнится.
const CAPACITY: usize = 1_024;

/// Задержка одного кадра из колонок CSV. `None` — кадр не показан или метки негодные.
pub fn display_latency_ms(
    cpu_start_ms: Option<f64>,
    present_start_ms: Option<f64>,
    until_displayed_ms: Option<f64>,
) -> Option<f64> {
    let latency = present_start_ms? - cpu_start_ms? + until_displayed_ms?;
    (latency.is_finite() && latency > 0.0 && latency < MAX_LATENCY_MS).then_some(latency)
}

/// Задержки последних кадров с моментом, когда кадр пришёл.
#[derive(Debug, Clone, Default)]
pub struct LatencyWindow {
    samples: VecDeque<(Millis, f64)>,
}

impl LatencyWindow {
    pub fn record(&mut self, latency_ms: f64, now_ms: Millis) {
        if self.samples.len() == CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back((now_ms, latency_ms));
    }

    /// Среднее за [`LATENCY_WINDOW_MS`]. `None` — кадров с задержкой за это время не было.
    pub fn average(&mut self, now_ms: Millis) -> Option<f64> {
        let since = now_ms.saturating_sub(LATENCY_WINDOW_MS);
        while self.samples.front().is_some_and(|(at, _)| *at < since) {
            self.samples.pop_front();
        }
        if self.samples.is_empty() {
            return None;
        }
        let sum: f64 = self.samples.iter().map(|(_, latency)| latency).sum();
        Some(sum / self.samples.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_adds_up_from_three_columns() {
        assert_eq!(display_latency_ms(Some(100.0), Some(112.0), Some(15.0)), Some(27.0));
        assert_eq!(display_latency_ms(Some(100.0), Some(112.0), None), None, "кадр не показан");
        assert_eq!(display_latency_ms(Some(200.0), Some(100.0), Some(5.0)), None);
        assert_eq!(display_latency_ms(Some(0.0), Some(5_000.0), Some(1.0)), None);
    }

    #[test]
    fn the_average_covers_only_the_last_second() {
        let mut window = LatencyWindow::default();
        assert_eq!(window.average(0), None);
        window.record(40.0, 100);
        window.record(20.0, 1_500);
        window.record(30.0, 1_900);
        assert_eq!(window.average(2_000), Some(25.0), "кадр из 100 мс устарел");
        assert_eq!(window.average(5_000), None);
    }

    #[test]
    fn the_window_is_bounded() {
        let mut window = LatencyWindow::default();
        for index in 0..5_000 {
            window.record(10.0, index / 10);
        }
        assert!(window.samples.len() <= CAPACITY);
    }
}

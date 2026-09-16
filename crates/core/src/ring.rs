//! Ограниченная история frametime.
//!
//! Окно ограничено **и** временем, **и** ёмкостью: игра на 500 FPS иначе растит историю без
//! предела, а игра на 30 FPS за те же 16 тысяч кадров накопила бы девять минут.
//!
//! Отличие от `FrametimeTracker.kt` старого проекта: там вытеснение по времени срабатывало
//! только внутри `record`. Стоило кадрам прекратиться — и последние тридцать секунд висели в
//! окне вечно, а UI показывал среднее по кадрам, которых давно нет. Здесь вытеснение — это
//! отдельная операция [`FrametimeRing::expire`], и все читающие методы выполняют её сами,
//! получая текущее время.

use crate::Millis;
use crate::statistics::{self, FrameStatistics};

/// Окно по умолчанию — 30 с.
pub const DEFAULT_WINDOW_MS: Millis = 30_000;
/// Ёмкость по умолчанию — 16384 кадра.
pub const DEFAULT_CAPACITY: usize = 16_384;

/// Кольцевой буфер frametime со временем записи каждого значения.
#[derive(Debug, Clone)]
pub struct FrametimeRing {
    frametimes: Box<[f32]>,
    timestamps: Box<[Millis]>,
    head: usize,
    len: usize,
    window_ms: Millis,
}

impl Default for FrametimeRing {
    fn default() -> Self {
        Self::new()
    }
}

impl FrametimeRing {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_WINDOW_MS, DEFAULT_CAPACITY)
    }

    /// `capacity` меньше единицы бессмыслен, поэтому поднимается до 1: буфер нулевого размера
    /// пришлось бы проверять на каждой операции ради случая, который никому не нужен.
    pub fn with_limits(window_ms: Millis, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            frametimes: vec![0.0; capacity].into_boxed_slice(),
            timestamps: vec![0; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
            window_ms,
        }
    }

    pub fn capacity(&self) -> usize {
        self.frametimes.len()
    }

    pub fn window_ms(&self) -> Millis {
        self.window_ms
    }

    /// Сколько значений сейчас в окне. Без вытеснения по времени — см. [`Self::len_at`].
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Длина окна на момент `now` — с вытеснением устаревших.
    pub fn len_at(&mut self, now_ms: Millis) -> usize {
        self.expire(now_ms);
        self.len
    }

    /// Записывает кадр. Возвращает `false`, если значение негодное и было отброшено.
    ///
    /// Отбраковка здесь же, а не только в статистике: незачем тратить место окна на значения,
    /// которые всё равно не попадут в расчёт.
    pub fn record(&mut self, frametime_ms: f64, at_ms: Millis) -> bool {
        if !frametime_ms.is_finite() || frametime_ms <= 0.0 {
            return false;
        }
        let capacity = self.capacity();
        let index = (self.head + self.len) % capacity;
        self.frametimes[index] = frametime_ms as f32;
        self.timestamps[index] = at_ms;
        if self.len == capacity {
            // Ёмкость исчерпана — самый старый кадр уходит.
            self.head = (self.head + 1) % capacity;
        } else {
            self.len += 1;
        }
        self.expire(at_ms);
        true
    }

    /// Выбрасывает всё, что старше окна относительно `now_ms`.
    ///
    /// Вызывать можно и нужно без новых кадров: именно это отличает «замер идёт, но кадров нет»
    /// от «показываю среднее по тому, чего уже нет».
    pub fn expire(&mut self, now_ms: Millis) {
        let cutoff = now_ms.saturating_sub(self.window_ms);
        let capacity = self.capacity();
        while self.len > 0 && self.timestamps[self.head] < cutoff {
            self.head = (self.head + 1) % capacity;
            self.len -= 1;
        }
    }

    /// Очищает историю целиком — при смене цели, чтобы цифры прошлой игры не «жили» в новой.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Копирует окно в хронологическом порядке. `out` очищается.
    pub fn copy_ordered_into(&self, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(self.len);
        let capacity = self.capacity();
        for i in 0..self.len {
            out.push(self.frametimes[(self.head + i) % capacity]);
        }
    }

    /// Не более `max` последних значений, старейшее первым. Для графика frametime.
    pub fn copy_recent_into(&self, out: &mut Vec<f32>, max: usize) {
        out.clear();
        let take = self.len.min(max);
        out.reserve(take);
        let capacity = self.capacity();
        let start = self.len - take;
        for i in 0..take {
            out.push(self.frametimes[(self.head + start + i) % capacity]);
        }
    }

    /// Статистика по окну на момент `now_ms`, с вытеснением устаревших.
    ///
    /// `scratch` и `ordered` переиспользуются между вызовами, чтобы опрос ничего не выделял.
    pub fn statistics(
        &mut self,
        now_ms: Millis,
        ordered: &mut Vec<f32>,
        scratch: &mut Vec<f32>,
    ) -> FrameStatistics {
        self.expire(now_ms);
        if self.len == 0 {
            // `ordered` читают и после вызова — график; кадров прошлого окна там быть не должно.
            ordered.clear();
            return FrameStatistics::EMPTY;
        }
        self.copy_ordered_into(ordered);
        statistics::compute_with_scratch(ordered, scratch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Записывает `count` кадров по `step_ms` каждый, начиная с `from_ms`.
    fn fill(ring: &mut FrametimeRing, count: usize, step_ms: Millis, from_ms: Millis) -> Millis {
        let mut at = from_ms;
        for _ in 0..count {
            at += step_ms;
            ring.record(step_ms as f64, at);
        }
        at
    }

    #[test]
    fn records_and_reports_in_chronological_order() {
        let mut ring = FrametimeRing::with_limits(10_000, 8);
        ring.record(10.0, 100);
        ring.record(20.0, 200);
        ring.record(30.0, 300);

        let mut out = Vec::new();
        ring.copy_ordered_into(&mut out);
        assert_eq!(out, vec![10.0, 20.0, 30.0]);
    }

    #[test]
    fn invalid_frametimes_never_enter_the_window() {
        let mut ring = FrametimeRing::with_limits(10_000, 8);
        assert!(!ring.record(f64::NAN, 100));
        assert!(!ring.record(0.0, 100));
        assert!(!ring.record(-5.0, 100));
        assert!(!ring.record(f64::INFINITY, 100));
        assert!(ring.is_empty());
        assert!(ring.record(16.0, 100));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn capacity_evicts_the_oldest() {
        let mut ring = FrametimeRing::with_limits(10_000_000, 4);
        for i in 1..=6u64 {
            ring.record(i as f64, i * 10);
        }
        let mut out = Vec::new();
        ring.copy_ordered_into(&mut out);
        assert_eq!(out, vec![3.0, 4.0, 5.0, 6.0], "остаются четыре последних");
        assert_eq!(ring.len(), 4);
    }

    #[test]
    fn time_evicts_beyond_the_window() {
        let mut ring = FrametimeRing::with_limits(1_000, 64);
        ring.record(16.0, 100);
        ring.record(16.0, 500);
        ring.record(16.0, 1_400); // окно теперь 400..1400 — первый кадр выпал
        assert_eq!(ring.len(), 2);
    }

    /// Главное отличие от старой реализации: если кадры прекратились, окно обязано опустеть
    /// само. Иначе пауза «замораживает» последние тридцать секунд навсегда.
    #[test]
    fn the_window_empties_without_any_writes() {
        let mut ring = FrametimeRing::with_limits(1_000, 64);
        fill(&mut ring, 10, 16, 0);
        assert_eq!(ring.len(), 10);

        // Ни одной новой записи — только течение времени.
        assert_eq!(ring.len_at(1_000), 10, "окно ещё покрывает записанное");
        assert_eq!(ring.len_at(1_200), 0, "кадры вышли за окно и должны исчезнуть");
    }

    #[test]
    fn statistics_reflect_expiry_not_just_writes() {
        let mut ring = FrametimeRing::with_limits(1_000, 64);
        fill(&mut ring, 20, 10, 0);

        let (mut ordered, mut scratch) = (Vec::new(), Vec::new());
        let live = ring.statistics(200, &mut ordered, &mut scratch);
        assert!(live.has_samples());

        let stale = ring.statistics(10_000, &mut ordered, &mut scratch);
        assert!(!stale.has_samples(), "после паузы статистики быть не должно");
        assert_eq!(stale, FrameStatistics::EMPTY);
    }

    #[test]
    fn clearing_drops_the_previous_game() {
        let mut ring = FrametimeRing::with_limits(10_000, 64);
        fill(&mut ring, 50, 16, 0);
        ring.clear();
        assert!(ring.is_empty());

        // И после очистки буфер остаётся рабочим, а не «съезжает» по индексам.
        ring.record(16.0, 5_000);
        let mut out = Vec::new();
        ring.copy_ordered_into(&mut out);
        assert_eq!(out, vec![16.0]);
    }

    #[test]
    fn recent_returns_the_tail_oldest_first() {
        let mut ring = FrametimeRing::with_limits(10_000_000, 8);
        for i in 1..=8u64 {
            ring.record(i as f64, i * 10);
        }
        let mut out = Vec::new();
        ring.copy_recent_into(&mut out, 3);
        assert_eq!(out, vec![6.0, 7.0, 8.0]);

        // Запрос больше, чем есть, отдаёт всё имеющееся и не паникует.
        ring.copy_recent_into(&mut out, 100);
        assert_eq!(out.len(), 8);
    }

    #[test]
    fn wrapping_keeps_order_after_many_rounds() {
        let mut ring = FrametimeRing::with_limits(10_000_000, 4);
        for i in 1..=100u64 {
            ring.record(i as f64, i * 10);
        }
        let mut out = Vec::new();
        ring.copy_ordered_into(&mut out);
        assert_eq!(out, vec![97.0, 98.0, 99.0, 100.0]);
    }

    #[test]
    fn a_zero_capacity_request_still_yields_a_usable_ring() {
        let mut ring = FrametimeRing::with_limits(1_000, 0);
        assert_eq!(ring.capacity(), 1);
        ring.record(16.0, 10);
        ring.record(17.0, 20);
        let mut out = Vec::new();
        ring.copy_ordered_into(&mut out);
        assert_eq!(out, vec![17.0], "помещается ровно один, самый свежий");
    }
}

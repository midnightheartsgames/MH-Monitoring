//! Источник монотонного времени.
//!
//! Часы вынесены за трейт по той же причине, по какой `core` принимает время аргументом: логика,
//! построенная на системных часах, ломается при переводе времени и не проверяется тестом иначе
//! как ожиданием в реальном времени.
//!
//! Отличие от `core` в том, что здесь часы нужны **потоку чтения**, которому никто не передаёт
//! `now_ms` извне: он записывает кадры по мере их прихода.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use mh_core::Millis;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> Millis;
}

/// Настоящие монотонные часы. Отсчёт идёт от момента создания.
#[derive(Debug)]
pub struct MonotonicClock {
    origin: Instant,
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self { origin: Instant::now() }
    }

    pub fn shared() -> Arc<dyn Clock> {
        Arc::new(Self::new())
    }
}

impl Clock for MonotonicClock {
    fn now_ms(&self) -> Millis {
        self.origin.elapsed().as_millis() as Millis
    }
}

/// Часы, которыми управляет тест.
///
/// `AtomicU64`, а не `Mutex`: часы читает поток чтения, а двигает поток теста, и блокировка
/// здесь создала бы порядок, которого в бою нет.
#[derive(Debug, Default)]
pub struct ManualClock {
    now_ms: AtomicU64,
}

impl ManualClock {
    pub fn new(start_ms: Millis) -> Self {
        Self { now_ms: AtomicU64::new(start_ms) }
    }

    pub fn set(&self, now_ms: Millis) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, delta_ms: Millis) {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> Millis {
        self.now_ms.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_clock_moves_only_when_told() {
        let clock = ManualClock::new(1_000);
        assert_eq!(clock.now_ms(), 1_000);
        clock.advance(500);
        assert_eq!(clock.now_ms(), 1_500);
        clock.set(42);
        assert_eq!(clock.now_ms(), 42);
    }

    #[test]
    fn a_monotonic_clock_never_goes_backwards() {
        let clock = MonotonicClock::new();
        let first = clock.now_ms();
        let second = clock.now_ms();
        assert!(second >= first);
    }
}

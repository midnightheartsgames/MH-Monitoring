//! Выбор цели на стороне пользователя.
//!
//! Живёт в UI, а не в движке: служба работает в сессии 0 и окна в фокусе у пользователя не
//! видит (`GetForegroundWindow` там пуст). UI решает, кого мерить, и отдаёт решение движку —
//! своему или службе.

use mh_core::{Millis, TargetResolution, TargetSettings, TargetTracker};
use mh_platform::process::{SystemProcessLookup, foreground_process};

pub struct TargetWatcher {
    tracker: TargetTracker,
    lookup: SystemProcessLookup,
    own_pid: u32,
}

impl Default for TargetWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl TargetWatcher {
    pub fn new() -> Self {
        Self {
            tracker: TargetTracker::new(),
            lookup: SystemProcessLookup,
            own_pid: std::process::id(),
        }
    }

    /// Решение на момент `now_ms` по настройкам пользователя.
    pub fn resolve(&mut self, now_ms: Millis, settings: &TargetSettings) -> TargetResolution {
        // Своё окно — оверлей или настройки — целью не бывает.
        let foreground = foreground_process(self.own_pid);
        self.tracker.resolve(now_ms, foreground.as_ref(), settings, &self.lookup)
    }
}

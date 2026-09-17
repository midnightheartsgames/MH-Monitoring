//! Выбор цели на стороне пользователя.
//!
//! Живёт в UI, а не в движке: служба работает в сессии 0 и окна в фокусе у пользователя не
//! видит (`GetForegroundWindow` там пуст). UI решает, кого мерить, и отдаёт решение движку —
//! своему или службе.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use mh_core::{
    Millis, SwitchGuard, TargetProcess, TargetResolution, TargetSettings, TargetTracker,
};
use mh_platform::pdh::CounterQuery;
use mh_platform::process::{SystemProcessLookup, foreground_process};
use mh_sources::hardware::gpu_counters::process_3d_load;

/// С какой загрузки 3D-движков процесс считается рисующим. Игра, даже в меню, выше; браузер и
/// редактор в покое — ниже.
const DRAWING_LOAD_PERCENT: f64 = 10.0;
/// Чаще счётчик снимать незачем: загрузка — среднее между двумя снимками.
const MIN_COLLECT_INTERVAL: Duration = Duration::from_millis(200);

pub struct TargetWatcher {
    tracker: TargetTracker,
    lookup: SystemProcessLookup,
    own_pid: u32,
    activity: RefCell<GraphicsActivity>,
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
            activity: RefCell::new(GraphicsActivity::default()),
        }
    }

    /// Решение на момент `now_ms` по настройкам пользователя. `target_has_frames` — у нынешней
    /// цели идут кадры: тогда её отдаёт только рисующий кандидат.
    pub fn resolve(
        &mut self,
        now_ms: Millis,
        settings: &TargetSettings,
        target_has_frames: bool,
    ) -> TargetResolution {
        // Своё окно — оверлей или настройки — целью не бывает.
        let foreground = foreground_process(self.own_pid);
        let activity = &self.activity;
        let draws = |candidate: &TargetProcess| activity.borrow_mut().draws(candidate.pid);
        let guard = SwitchGuard { target_has_frames, candidate_draws: &draws };
        self.tracker.resolve_guarded(now_ms, foreground.as_ref(), settings, &self.lookup, &guard)
    }
}

/// Загрузка 3D-движков видеокарты по процессам — счётчик PDH `GPU Engine`.
///
/// Открывается при первом вопросе и снимается только тогда, когда спрашивают: в обычной работе
/// счётчик с сотнями экземпляров не опрашивается.
#[derive(Default)]
struct GraphicsActivity {
    query: Option<CounterQuery>,
    unavailable: bool,
    collected_at: Option<Instant>,
}

impl GraphicsActivity {
    fn draws(&mut self, pid: u32) -> bool {
        if self.unavailable {
            // Без счётчика не узнать — прежнее поведение: смена разрешена.
            return true;
        }
        if self.query.is_none() {
            match CounterQuery::open(&[r"\GPU Engine(*)\Utilization Percentage"]) {
                Ok(query) => self.query = Some(query),
                Err(_) => {
                    self.unavailable = true;
                    return true;
                }
            }
        }
        let Some(query) = self.query.as_mut() else { return true };
        let now = Instant::now();
        if self.collected_at.is_none_or(|at| now - at >= MIN_COLLECT_INTERVAL) {
            let _ = query.collect();
            self.collected_at = Some(now);
        }
        // До второго снимка значений нет — это «пока не рисует»: следующий опрос спросит снова.
        process_3d_load(&query.instances(0), pid) >= DRAWING_LOAD_PERCENT
    }
}

//! Политика выбора источника и отката — на подставных фабриках.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use mh_core::FrameStatistics;

use super::*;

/// Подставная фабрика: её источники сообщают тот статус, что выставил тест.
struct FakeFactory {
    kind: SourceKind,
    status: Arc<Mutex<FrameStatus>>,
    start_error: Mutex<Option<Failure>>,
    starts: AtomicUsize,
    stops: Arc<AtomicUsize>,
    targets: Mutex<Vec<u32>>,
}

impl FakeFactory {
    fn new(kind: SourceKind) -> Arc<Self> {
        Arc::new(Self {
            kind,
            status: Arc::new(Mutex::new(FrameStatus::Measuring)),
            start_error: Mutex::new(None),
            starts: AtomicUsize::new(0),
            stops: Arc::new(AtomicUsize::new(0)),
            targets: Mutex::new(Vec::new()),
        })
    }

    fn set_status(&self, status: FrameStatus) {
        *self.status.lock().unwrap() = status;
    }

    fn fail_to_start(&self, failure: Failure) {
        *self.start_error.lock().unwrap() = Some(failure);
    }

    fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }

    fn stops(&self) -> usize {
        self.stops.load(Ordering::SeqCst)
    }
}

impl FrameSourceFactory for FakeFactory {
    fn kind(&self) -> SourceKind {
        self.kind
    }

    fn start(&self, target: u32, _session_id: u64) -> Result<Box<dyn FrameSource>, Failure> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.targets.lock().unwrap().push(target);
        if let Some(failure) = self.start_error.lock().unwrap().clone() {
            return Err(failure);
        }
        Ok(Box::new(FakeSource {
            kind: self.kind,
            status: Arc::clone(&self.status),
            stops: Arc::clone(&self.stops),
            stopped: false,
        }))
    }
}

struct FakeSource {
    kind: SourceKind,
    status: Arc<Mutex<FrameStatus>>,
    stops: Arc<AtomicUsize>,
    stopped: bool,
}

impl FrameSource for FakeSource {
    fn kind(&self) -> SourceKind {
        self.kind
    }

    fn poll(&mut self, _now_ms: Millis) -> FrameReport {
        let status = self.status.lock().unwrap().clone();
        let statistics = if status == FrameStatus::Measuring {
            FrameStatistics {
                average_fps: Some(144.0),
                sample_count: 500,
                ..FrameStatistics::EMPTY
            }
        } else {
            FrameStatistics::EMPTY
        };
        FrameReport {
            status,
            statistics,
            last_frame_at_ms: Some(1_000),
            diagnostics: format!("подставной {:?}", self.kind),
            presentation: None,
        }
    }

    fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn game(pid: u32, started_at_ms: Millis) -> TargetProcess {
    TargetProcess::new(pid, "game.exe", Some(started_at_ms))
}

struct Rig {
    primary: Arc<FakeFactory>,
    fallback: Arc<FakeFactory>,
    capture: FrameCapture,
}

fn rig() -> Rig {
    let primary = FakeFactory::new(SourceKind::PresentMon);
    let fallback = FakeFactory::new(SourceKind::OwnEtw);
    let capture = FrameCapture::new(
        Arc::clone(&primary) as Arc<dyn FrameSourceFactory>,
        Some(Arc::clone(&fallback) as Arc<dyn FrameSourceFactory>),
    );
    Rig { primary, fallback, capture }
}

// --- без цели -----------------------------------------------------------------------------

#[test]
fn without_a_target_nothing_is_started() {
    let mut rig = rig();
    let state = rig.capture.poll(0);
    assert_eq!(state.availability, FpsAvailability::Waiting);
    assert_eq!(state.reason, Some(FpsReason::NoTarget));
    assert_eq!(rig.primary.starts(), 0);
}

// --- основной источник --------------------------------------------------------------------

#[test]
fn a_healthy_primary_is_used_and_carries_no_note() {
    let mut rig = rig();
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert!(state.is_delivering());
    assert_eq!(state.statistics.average_fps, Some(144.0));
    assert_eq!(state.detail, None, "на основном источнике оговорок нет");
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::PresentMon));
    assert_eq!(rig.fallback.starts(), 0);
}

/// Заголовок пришёл, кадров ещё нет — это ожидание, а не доступность (PLAN.md §2.7).
#[test]
fn starting_is_waiting_not_available() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::Starting);
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(state.availability, FpsAvailability::Waiting);
    assert_eq!(state.reason, Some(FpsReason::NoFrames));
    assert_eq!(rig.fallback.starts(), 0, "ожидание первого кадра — не повод для отката");
}

/// Прерывание идущего потока — не повод менять источник: игра могла встать на паузу.
#[test]
fn a_stall_does_not_trigger_a_fallback() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::Stalled);
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(state.reason, Some(FpsReason::FramesStalled));
    assert_eq!(rig.fallback.starts(), 0);
}

// --- откат --------------------------------------------------------------------------------

#[test]
fn no_frames_from_the_primary_switches_to_the_fallback() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::NoFrames);
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::OwnEtw));
    assert_eq!(rig.primary.stops(), 1, "основной источник остановлен до запуска запасного");
    assert!(state.is_delivering());
}

/// Требование плана: при откате пользователь явно видит, что источник сменился и чего тот не
/// покрывает. После измерения в P0 это OpenGL, а не Vulkan.
#[test]
fn the_fallback_is_announced_with_what_it_cannot_see() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::NoFrames);
    rig.fallback.set_status(FrameStatus::NoFrames);
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    let note = state.detail.clone().expect("откат обязан быть объявлен");
    assert!(note.contains("собственный ETW"), "{note}");
    assert!(note.contains("PresentMon"), "{note}");
    assert!(note.contains("OpenGL"), "{note}");
    assert!(!note.contains("Vulkan"), "Vulkan этим источником виден: {note}");
    assert_eq!(state.message(), Some(note.as_str()), "HUD показывает именно эту строку");
}

/// Запасной источник считает кадры этой игры — значит, она ему видна, и про OpenGL не говорим.
#[test]
fn a_measuring_fallback_does_not_warn_about_what_it_cannot_see() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::NoFrames);
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    let note = state.detail.clone().expect("откат по-прежнему объявлен");
    assert!(note.contains("собственный ETW"), "{note}");
    assert!(!note.contains("OpenGL"), "{note}");
}

#[test]
fn an_unsupported_csv_switches_to_the_fallback() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::Failed(Failure::new(FpsReason::UnsupportedCsv)));
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::OwnEtw));
    assert!(state.detail.unwrap().contains(FpsReason::UnsupportedCsv.message()));
}

/// DMC4 в окне: PresentMon видит 2–25 FPS из 180 — такой счёт хуже отсутствующего (§2.16).
#[test]
fn an_untracked_present_mode_switches_to_the_fallback() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::Failed(Failure::with_detail(
        FpsReason::PresentModeUntracked,
        "режим вывода «Composed: Copy with GPU GDI»",
    )));
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::OwnEtw));
    assert!(state.is_delivering());
    // Кадры идут — короткая строка без предупреждений (DMC4 SE в окне).
    assert_eq!(state.message(), Some("кадры считает собственный ETW (вывод через GDI)"));
}

#[test]
fn a_primary_that_cannot_start_switches_to_the_fallback() {
    let mut rig = rig();
    rig.primary.fail_to_start(Failure::with_detail(FpsReason::ExecutableMissing, "нет файла"));
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::OwnEtw));
    assert!(state.is_delivering());
}

/// Собственному потребителю нужны ровно те же права (PLAN.md §2.3). Откат лишь спрятал бы
/// настоящую причину за другой.
#[test]
fn missing_rights_never_trigger_a_fallback() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::Failed(Failure::new(FpsReason::NotPermitted)));
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(state.availability, FpsAvailability::Error);
    assert_eq!(state.reason, Some(FpsReason::NotPermitted));
    assert_eq!(rig.fallback.starts(), 0);
}

#[test]
fn a_start_refused_for_rights_is_reported_as_unavailable() {
    let mut rig = rig();
    rig.primary.fail_to_start(Failure::new(FpsReason::NotPermitted));
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(state.availability, FpsAvailability::Unavailable);
    assert_eq!(state.reason, Some(FpsReason::NotPermitted));
    assert_eq!(rig.fallback.starts(), 0);
}

#[test]
fn when_the_fallback_fails_too_its_reason_is_reported() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::NoFrames);
    rig.fallback.fail_to_start(Failure::with_detail(FpsReason::BackendFailed, "сессия не создана"));
    rig.capture.set_target(Some(&game(4242, 1)));

    let state = rig.capture.poll(0);
    assert_eq!(state.availability, FpsAvailability::Error);
    assert_eq!(state.reason, Some(FpsReason::BackendFailed));
    assert_eq!(state.detail.as_deref(), Some("сессия не создана"));
}

/// Когда идти дальше некуда, результат фиксируется: перезапускать тот же провал на каждом опросе
/// бессмысленно, а каждый запуск PresentMon — это новый процесс и новая ETW-сессия.
#[test]
fn an_exhausted_target_is_not_restarted_on_every_poll() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::NoFrames);
    rig.fallback.fail_to_start(Failure::new(FpsReason::BackendFailed));
    rig.capture.set_target(Some(&game(4242, 1)));

    for tick in 0..20 {
        rig.capture.poll(tick * 250);
    }
    assert_eq!(rig.primary.starts(), 1);
    assert_eq!(rig.fallback.starts(), 1);
}

/// Метаться между источниками на одной игре — значит перезапускать захват по кругу.
#[test]
fn a_delivering_fallback_is_sticky_for_the_same_target() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::NoFrames);
    rig.capture.set_target(Some(&game(4242, 1)));
    rig.capture.poll(0);

    // Даже если бы основной источник «починился», пока запасной даёт кадры, основной не пробуется.
    rig.primary.set_status(FrameStatus::Measuring);
    for tick in 1..10 {
        rig.capture.poll(tick * 250);
    }
    assert_eq!(rig.primary.starts(), 1);
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::OwnEtw));
}

/// Ion Fury (OpenGL): PresentMon не успел за первые 5 с, собственный ETW OpenGL не видит вовсе.
/// Застрять на запасном навсегда — значит так и не показать FPS.
#[test]
fn a_silent_fallback_hands_the_target_back_to_the_primary() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::NoFrames);
    rig.fallback.set_status(FrameStatus::NoFrames);
    rig.capture.set_target(Some(&game(4242, 1)));

    rig.capture.poll(0); // основной молчит → запасной
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::OwnEtw));
    rig.capture.poll(250); // запасной тоже молчит → обратно
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::PresentMon));
    assert_eq!(rig.primary.starts(), 2);

    // Второй раз основной не бросаем: он ждёт кадров, сколько потребуется.
    for tick in 2..20 {
        rig.capture.poll(tick * 250);
    }
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::PresentMon));
    assert_eq!(rig.primary.starts(), 2);
    assert_eq!(rig.fallback.starts(), 1);

    rig.primary.set_status(FrameStatus::Measuring);
    let state = rig.capture.poll(10_000);
    assert!(state.is_delivering());
    assert_eq!(state.detail, None, "вернувшись на основной, об откате больше не говорим");
}

/// Откат по иной причине (например, неподдерживаемый режим вывода) назад не возвращается:
/// основной источник там не молчит, он считает неправильно.
#[test]
fn a_fallback_for_a_real_failure_stays_even_if_silent() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::Failed(Failure::new(FpsReason::PresentModeUntracked)));
    rig.fallback.set_status(FrameStatus::NoFrames);
    rig.capture.set_target(Some(&game(4242, 1)));
    for tick in 0..10 {
        rig.capture.poll(tick * 250);
    }
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::OwnEtw));
    assert_eq!(rig.primary.starts(), 1);
}

#[test]
fn without_a_configured_fallback_the_primary_failure_surfaces() {
    let primary = FakeFactory::new(SourceKind::PresentMon);
    primary.set_status(FrameStatus::Failed(Failure::new(FpsReason::UnsupportedCsv)));
    let mut capture = FrameCapture::new(Arc::clone(&primary) as Arc<dyn FrameSourceFactory>, None);
    capture.set_target(Some(&game(4242, 1)));

    let state = capture.poll(0);
    assert_eq!(state.reason, Some(FpsReason::UnsupportedCsv));
    assert_eq!(state.availability, FpsAvailability::Error);
}

// --- смена цели ---------------------------------------------------------------------------

#[test]
fn the_same_run_set_twice_does_not_restart_the_capture() {
    let mut rig = rig();
    rig.capture.set_target(Some(&game(4242, 1)));
    rig.capture.poll(0);
    rig.capture.set_target(Some(&game(4242, 1)));
    rig.capture.poll(250);
    assert_eq!(rig.primary.starts(), 1);
    assert_eq!(rig.primary.stops(), 0);
}

/// Windows переиспользует PID: тот же номер с другим временем старта — новая игра.
#[test]
fn a_reused_pid_is_a_new_capture() {
    let mut rig = rig();
    rig.capture.set_target(Some(&game(4242, 1)));
    let first = rig.capture.poll(0).session_id;

    rig.capture.set_target(Some(&game(4242, 9_000)));
    let second = rig.capture.poll(250).session_id;

    assert_eq!(rig.primary.starts(), 2);
    assert_eq!(rig.primary.stops(), 1, "прошлый сеанс остановлен");
    assert!(second > first, "у нового сеанса новый номер");
}

/// Новая игра заслуживает лучшего источника — откат прошлой цели на неё не переносится.
#[test]
fn a_new_target_starts_again_from_the_primary() {
    let mut rig = rig();
    rig.primary.set_status(FrameStatus::NoFrames);
    rig.capture.set_target(Some(&game(4242, 1)));
    rig.capture.poll(0);
    assert_eq!(rig.capture.active_kind(), Some(SourceKind::OwnEtw));

    rig.primary.set_status(FrameStatus::Measuring);
    rig.capture.set_target(Some(&game(777, 1)));
    let state = rig.capture.poll(250);

    assert_eq!(rig.capture.active_kind(), Some(SourceKind::PresentMon));
    assert_eq!(state.detail, None, "заметка об откате к новой игре не относится");
    assert_eq!(*rig.primary.targets.lock().unwrap(), vec![4242, 777]);
}

#[test]
fn a_new_target_clears_a_parked_failure() {
    let mut rig = rig();
    rig.primary.fail_to_start(Failure::new(FpsReason::NotPermitted));
    rig.capture.set_target(Some(&game(4242, 1)));
    assert_eq!(rig.capture.poll(0).reason, Some(FpsReason::NotPermitted));

    // Права выдали — следующая цель обязана попробовать заново, а не вспомнить старый провал.
    *rig.primary.start_error.lock().unwrap() = None;
    rig.capture.set_target(Some(&game(777, 1)));
    assert!(rig.capture.poll(250).is_delivering());
}

#[test]
fn losing_the_target_stops_the_capture() {
    let mut rig = rig();
    rig.capture.set_target(Some(&game(4242, 1)));
    rig.capture.poll(0);

    rig.capture.set_target(None);
    let state = rig.capture.poll(250);
    assert_eq!(state.reason, Some(FpsReason::NoTarget));
    assert_eq!(rig.primary.stops(), 1);
    assert_eq!(rig.capture.active_kind(), None);
}

/// Требование §8, но на уровне политики: полсотни смен цели — и ни одного незакрытого источника.
#[test]
fn fifty_target_switches_leave_nothing_running() {
    let mut rig = rig();
    for pid in 0..50u32 {
        rig.capture.set_target(Some(&game(1_000 + pid, 1)));
        rig.capture.poll(pid as Millis * 100);
    }
    rig.capture.set_target(None);
    rig.capture.poll(10_000);

    assert_eq!(rig.primary.starts(), 50);
    assert_eq!(rig.primary.stops(), 50, "каждый запущенный источник остановлен");
}

#[test]
fn dropping_the_capture_stops_the_active_source() {
    let rig = rig();
    let Rig { primary, fallback: _, mut capture } = rig;
    capture.set_target(Some(&game(4242, 1)));
    capture.poll(0);
    drop(capture);
    assert_eq!(primary.stops(), 1);
}

#[test]
fn the_diagnostics_of_the_active_source_are_available() {
    let mut rig = rig();
    assert_eq!(rig.capture.diagnostics(), None, "до первого опроса сказать нечего");
    rig.capture.set_target(Some(&game(4242, 1)));
    rig.capture.poll(0);
    assert_eq!(rig.capture.diagnostics(), Some("подставной PresentMon"));
}

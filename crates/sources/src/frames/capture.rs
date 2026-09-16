//! Захват кадров текущей цели с откатом на запасной источник (PLAN.md §6/P2).
//!
//! Политика:
//!
//! * **PresentMon — основной.** Он видит больше графических API и даёт метрики, которых у
//!   собственного потребителя нет вовсе (решение D1).
//! * **Откат на собственный ETW** — при неподдерживаемой схеме CSV, при отсутствии кадров дольше
//!   watchdog'а и при невозможности запустить PresentMon.
//! * **Отказ в правах отката не вызывает.** Собственный потребитель требует ровно тех же прав
//!   (PLAN.md §2.3), и переключение только спрятало бы настоящую причину.
//! * **Откат явный.** Пока замер идёт через запасной источник, пользователь видит, что источник
//!   сменён и что им не покрывается. Это OpenGL, а не Vulkan, как предполагалось до измерения.
//! * **Откат липкий в пределах цели.** Метаться между источниками на одной игре — значит
//!   перезапускать захват по кругу. Новая цель снова начинает с основного.
//!
//! Модуль чистый: источники приходят через [`FrameSourceFactory`], и вся политика проверяется
//! подставными фабриками.

use std::sync::Arc;

use mh_core::{FpsAvailability, FpsReason, FpsState, Millis, TargetProcess};

use super::source::{
    Failure, FrameReport, FrameSource, FrameSourceFactory, FrameStatus, SourceKind,
};

pub struct FrameCapture {
    primary: Arc<dyn FrameSourceFactory>,
    fallback: Option<Arc<dyn FrameSourceFactory>>,
    target: Option<TargetProcess>,
    active: Option<Box<dyn FrameSource>>,
    on_fallback: bool,
    /// Почему ушли с основного источника — для сообщения пользователю.
    fallback_cause: Option<FpsReason>,
    /// Итог, дальше которого для этой цели идти некуда. Сбрасывается сменой цели.
    parked: Option<(Failure, SourceKind)>,
    session_id: u64,
    /// Счётчики последнего опрошенного источника.
    last_diagnostics: Option<String>,
}

impl FrameCapture {
    pub fn new(
        primary: Arc<dyn FrameSourceFactory>,
        fallback: Option<Arc<dyn FrameSourceFactory>>,
    ) -> Self {
        Self {
            primary,
            fallback,
            target: None,
            active: None,
            on_fallback: false,
            fallback_cause: None,
            parked: None,
            session_id: 0,
            last_diagnostics: None,
        }
    }

    /// Счётчики последнего опрошенного источника — для лога и диагностики, когда кадров нет.
    pub fn diagnostics(&self) -> Option<&str> {
        self.last_diagnostics.as_deref()
    }

    /// Источник, который работает прямо сейчас.
    pub fn active_kind(&self) -> Option<SourceKind> {
        self.active.as_ref().map(|source| source.kind())
    }

    /// Задаёт цель. Та же цель — тот же запуск — ничего не перезапускает.
    ///
    /// Сравнение идёт по [`TargetProcess::is_same_run_as`], а не по PID: переиспользованный
    /// Windows номер процесса — это другая игра и новый сеанс.
    pub fn set_target(&mut self, target: Option<&TargetProcess>) {
        let unchanged = match (&self.target, target) {
            (Some(current), Some(next)) => current.is_same_run_as(next),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        self.stop_active();
        self.target = target.cloned();
        // История и выбор источника принадлежат прошлой игре.
        self.on_fallback = false;
        self.fallback_cause = None;
        self.parked = None;
    }

    /// Опрос. Возвращает единственное состояние, которое публикуется дальше.
    pub fn poll(&mut self, now_ms: Millis) -> FpsState {
        let Some(target) = self.target.clone() else {
            self.stop_active();
            return FpsState {
                availability: FpsAvailability::Waiting,
                reason: Some(FpsReason::NoTarget),
                session_id: self.session_id,
                ..FpsState::INITIAL
            };
        };

        if self.parked.is_some() {
            return self.parked_state(&target);
        }

        if self.active.is_none() && !self.start(&target) {
            return self.parked_state(&target);
        }

        let report = self.poll_active(now_ms);

        if !self.on_fallback && self.fallback.is_some() && runtime_allows_fallback(&report.status) {
            let cause = cause_of(&report.status);
            self.stop_active();
            self.switch_to_fallback(cause);
            if !self.start(&target) {
                return self.parked_state(&target);
            }
            let report = self.poll_active(now_ms);
            return self.report_state(report, &target);
        }

        if let FrameStatus::Failed(failure) = &report.status {
            // Дальше идти некуда: либо это уже запасной, либо откат запрещён.
            let kind = self.active_kind().unwrap_or(SourceKind::PresentMon);
            self.parked = Some((failure.clone(), kind));
            let state = self.report_state(report, &target);
            self.stop_active();
            return state;
        }

        self.report_state(report, &target)
    }

    fn poll_active(&mut self, now_ms: Millis) -> FrameReport {
        let report = self.active.as_mut().expect("источник запущен выше").poll(now_ms);
        self.last_diagnostics = Some(report.diagnostics.clone());
        report
    }

    /// Запускает источник. `false` — запустить не вышло, и итог записан в `parked`.
    fn start(&mut self, target: &TargetProcess) -> bool {
        let factory = if self.on_fallback {
            Arc::clone(self.fallback.as_ref().expect("откат только при наличии запасного"))
        } else {
            Arc::clone(&self.primary)
        };
        self.session_id += 1;
        match factory.start(target.pid, self.session_id) {
            Ok(source) => {
                self.active = Some(source);
                true
            }
            Err(failure) => {
                if !self.on_fallback
                    && self.fallback.is_some()
                    && reason_allows_fallback(failure.reason)
                {
                    self.switch_to_fallback(failure.reason);
                    // Рекурсия не глубже одного уровня: `on_fallback` уже поднят.
                    return self.start(target);
                }
                self.parked = Some((failure, factory.kind()));
                false
            }
        }
    }

    fn switch_to_fallback(&mut self, cause: FpsReason) {
        self.on_fallback = true;
        self.fallback_cause = Some(cause);
    }

    fn stop_active(&mut self) {
        if let Some(mut source) = self.active.take() {
            source.stop();
        }
    }

    /// Строка о том, что замер идёт не через основной источник, и чего это стоит.
    fn fallback_note(&self, kind: SourceKind) -> Option<String> {
        if !self.on_fallback {
            return None;
        }
        let cause = self.fallback_cause.map(FpsReason::message).unwrap_or("не работает");
        let mut note = format!("{} вместо PresentMon ({cause})", kind.label());
        if let Some(gap) = kind.coverage_gap() {
            note.push_str(" — ");
            note.push_str(gap);
        }
        Some(note)
    }

    fn report_state(&self, report: FrameReport, target: &TargetProcess) -> FpsState {
        let kind = self.active_kind().unwrap_or(SourceKind::PresentMon);
        let (availability, reason, failure_detail) = match report.status {
            FrameStatus::Starting => (FpsAvailability::Waiting, Some(FpsReason::NoFrames), None),
            FrameStatus::Measuring => (FpsAvailability::Available, None, None),
            FrameStatus::Stalled => {
                (FpsAvailability::Waiting, Some(FpsReason::FramesStalled), None)
            }
            FrameStatus::NoFrames => (FpsAvailability::Waiting, Some(FpsReason::NoFrames), None),
            FrameStatus::Ended => (FpsAvailability::Waiting, Some(FpsReason::SessionEnded), None),
            FrameStatus::Failed(failure) => {
                (FpsAvailability::Error, Some(failure.reason), failure.detail)
            }
        };
        FpsState {
            availability,
            reason,
            // Слова источника об ошибке важнее заметки об откате: объясняют, что сломалось сейчас.
            detail: failure_detail.or_else(|| self.fallback_note(kind)),
            target: Some(target.clone()),
            statistics: report.statistics,
            session_id: self.session_id,
            last_frame_at_ms: report.last_frame_at_ms,
        }
    }

    fn parked_state(&self, target: &TargetProcess) -> FpsState {
        let (failure, kind) = self.parked.as_ref().expect("вызывается только при наличии итога");
        self.failure_state(failure, *kind, target)
    }

    fn failure_state(
        &self,
        failure: &Failure,
        kind: SourceKind,
        target: &TargetProcess,
    ) -> FpsState {
        FpsState {
            availability: match failure.reason {
                FpsReason::ExecutableMissing | FpsReason::NotPermitted => {
                    FpsAvailability::Unavailable
                }
                _ => FpsAvailability::Error,
            },
            reason: Some(failure.reason),
            detail: failure.detail.clone().or_else(|| self.fallback_note(kind)),
            target: Some(target.clone()),
            session_id: self.session_id,
            ..FpsState::INITIAL
        }
    }
}

impl Drop for FrameCapture {
    fn drop(&mut self) {
        self.stop_active();
    }
}

/// Разрешает ли причина отказа уйти на запасной источник.
///
/// Отказ в правах — нет: собственному потребителю нужны ровно те же права (PLAN.md §2.3), и
/// откат лишь подменил бы понятную причину на непонятную.
fn reason_allows_fallback(reason: FpsReason) -> bool {
    matches!(
        reason,
        FpsReason::UnsupportedCsv
            | FpsReason::LaunchFailed
            | FpsReason::ExecutableMissing
            | FpsReason::BackendFailed
            | FpsReason::SessionConflict
            | FpsReason::NoFrames
    )
}

fn runtime_allows_fallback(status: &FrameStatus) -> bool {
    match status {
        FrameStatus::NoFrames => true,
        FrameStatus::Failed(failure) => reason_allows_fallback(failure.reason),
        _ => false,
    }
}

fn cause_of(status: &FrameStatus) -> FpsReason {
    match status {
        FrameStatus::Failed(failure) => failure.reason,
        _ => FpsReason::NoFrames,
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;

//! PresentMon как [`FrameSource`].

use std::sync::Arc;

use mh_core::{FpsReason, Millis};

use crate::clock::Clock;
use crate::frames::source::{
    Failure, FrameReport, FrameSource, FrameSourceFactory, FrameStatus, SourceKind,
};

use super::child::CaptureLauncher;
use super::command::{self, ExecutableChoice};
use super::session::{CaptureSession, SessionStatus};

/// Остановка ETW-сессии по имени.
///
/// Хук, а не прямой вызов `mh-platform`: этот модуль собирается и тестируется на любой
/// платформе, а сама остановка возможна только на Windows.
pub type SessionCleanup = Arc<dyn Fn(&str) + Send + Sync>;

/// Запускает PresentMon для очередной цели.
pub struct PresentMonFactory {
    pub launcher: Arc<dyn CaptureLauncher>,
    pub executable: ExecutableChoice,
    /// Имя сессии — одно на запуск приложения (PLAN.md §2.1.1).
    pub session_name: String,
    pub clock: Arc<dyn Clock>,
    pub session_cleanup: Option<SessionCleanup>,
}

impl FrameSourceFactory for PresentMonFactory {
    fn kind(&self) -> SourceKind {
        SourceKind::PresentMon
    }

    fn start(
        &self,
        target_process_id: u32,
        session_id: u64,
    ) -> Result<Box<dyn FrameSource>, Failure> {
        // Отсутствующий exe — отдельный диагноз, а не безликий «не удалось запустить»: это
        // единственный случай, который пользователь может исправить сам, указав путь.
        if !self.executable.path().is_file() {
            return Err(Failure::with_detail(
                FpsReason::ExecutableMissing,
                self.executable.path().display().to_string(),
            ));
        }

        let command = command::build(&self.executable, &self.session_name, target_process_id);
        let session = CaptureSession::start(
            self.launcher.as_ref(),
            &command,
            Arc::clone(&self.clock),
            Some(target_process_id),
            session_id,
        )
        .map_err(|error| Failure::with_detail(FpsReason::LaunchFailed, error.to_string()))?;

        Ok(Box::new(PresentMonSource {
            session,
            session_name: self.session_name.clone(),
            cleanup: self.session_cleanup.clone(),
            stopped: false,
        }))
    }
}

struct PresentMonSource {
    session: CaptureSession,
    session_name: String,
    cleanup: Option<SessionCleanup>,
    stopped: bool,
}

impl FrameSource for PresentMonSource {
    fn kind(&self) -> SourceKind {
        SourceKind::PresentMon
    }

    fn poll(&mut self, now_ms: Millis) -> FrameReport {
        let report = self.session.poll(now_ms);
        let status = match report.status {
            SessionStatus::Starting => FrameStatus::Starting,
            SessionStatus::Measuring => FrameStatus::Measuring,
            SessionStatus::Stalled => FrameStatus::Stalled,
            SessionStatus::NoFrames => FrameStatus::NoFrames,
            SessionStatus::Failed(failure) => FrameStatus::Failed(failure),
            SessionStatus::Ended { .. } => FrameStatus::Ended,
        };
        let counters = report.counters;
        FrameReport {
            status,
            statistics: report.statistics,
            last_frame_at_ms: report.last_frame_at_ms,
            diagnostics: format!(
                "строк {}, кадров {}, негодных {}, чужих {}",
                counters.rows, counters.parsed, counters.rejected, counters.filtered
            ),
        }
    }

    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.session.stop();
        // Порядок обязателен. `stop` сеанса завершает PresentMon через `TerminateProcess`, и
        // ребёнок **не успевает** остановить свою ETW-сессию (PLAN.md §2.2). Без этой строки
        // каждая смена цели и каждый откат на запасной источник оставляли бы сироту, а сирота
        // обнуляет захват кадров на всей машине.
        if let Some(cleanup) = &self.cleanup {
            cleanup(&self.session_name);
        }
    }
}

impl Drop for PresentMonSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::frames::presentmon::child::fake::{FakeLauncher, Script};
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Любой заведомо существующий файл: фабрика проверяет только наличие, а запускает всё
    /// равно подставной запускальщик.
    fn existing_executable() -> ExecutableChoice {
        ExecutableChoice::Bundled(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
    }

    fn factory(
        launcher: Arc<FakeLauncher>,
        executable: ExecutableChoice,
        cleaned: Arc<Mutex<Vec<String>>>,
    ) -> PresentMonFactory {
        PresentMonFactory {
            launcher,
            executable,
            session_name: "MHMonitor-1234".to_string(),
            clock: Arc::new(ManualClock::new(0)),
            session_cleanup: Some(Arc::new(move |name: &str| {
                cleaned.lock().unwrap().push(name.to_string());
            })),
        }
    }

    #[test]
    fn a_missing_executable_is_its_own_diagnosis() {
        let cleaned = Arc::new(Mutex::new(Vec::new()));
        let launcher = Arc::new(FakeLauncher::new(Script::silent()));
        let factory = factory(
            Arc::clone(&launcher),
            ExecutableChoice::Bundled(PathBuf::from(r"Z:\нет\такого\PresentMon.exe")),
            cleaned,
        );

        match factory.start(4242, 1) {
            Err(failure) => assert_eq!(failure.reason, FpsReason::ExecutableMissing),
            Ok(_) => panic!("без exe запускаться нечему"),
        }
        assert_eq!(launcher.launches(), 0, "запускать отсутствующий файл даже не пытаемся");
    }

    #[test]
    fn a_launch_error_is_reported_as_launch_failed() {
        let cleaned = Arc::new(Mutex::new(Vec::new()));
        let launcher = Arc::new(FakeLauncher::failing(std::io::ErrorKind::PermissionDenied));
        let factory = factory(launcher, existing_executable(), cleaned);

        match factory.start(4242, 1) {
            Err(failure) => assert_eq!(failure.reason, FpsReason::LaunchFailed),
            Ok(_) => panic!("запуск обязан провалиться"),
        }
    }

    /// Главное требование адаптера: после остановки PresentMon его ETW-сессия гасится по имени,
    /// потому что сам ребёнок сделать этого не успевает (PLAN.md §2.2).
    #[test]
    fn stopping_also_stops_the_etw_session_by_name() {
        let cleaned = Arc::new(Mutex::new(Vec::new()));
        let launcher = Arc::new(FakeLauncher::new(Script::silent()));
        let factory = factory(Arc::clone(&launcher), existing_executable(), Arc::clone(&cleaned));

        let mut source = factory.start(4242, 1).unwrap_or_else(|f| panic!("запуск: {f:?}"));
        assert_eq!(source.kind(), SourceKind::PresentMon);
        source.stop();

        assert_eq!(launcher.killed(), 1, "ребёнок завершён");
        assert_eq!(*cleaned.lock().unwrap(), vec!["MHMonitor-1234".to_string()]);
    }

    /// Забытый `stop` не должен оставить сироту.
    #[test]
    fn dropping_the_source_cleans_up_exactly_once() {
        let cleaned = Arc::new(Mutex::new(Vec::new()));
        let launcher = Arc::new(FakeLauncher::new(Script::silent()));
        let factory = factory(launcher, existing_executable(), Arc::clone(&cleaned));

        {
            let mut source = factory.start(4242, 1).unwrap_or_else(|f| panic!("запуск: {f:?}"));
            source.stop();
            // Drop после явного stop не должен гасить сессию второй раз: за это время она уже
            // может принадлежать следующему источнику с тем же именем.
        }
        {
            let _forgotten = factory.start(4242, 2).unwrap_or_else(|f| panic!("запуск: {f:?}"));
        }
        assert_eq!(cleaned.lock().unwrap().len(), 2, "по одной уборке на источник, не больше");
    }

    #[test]
    fn a_silent_child_maps_to_starting() {
        let cleaned = Arc::new(Mutex::new(Vec::new()));
        let launcher = Arc::new(FakeLauncher::new(Script::silent()));
        let factory = factory(launcher, existing_executable(), cleaned);

        let mut source = factory.start(4242, 1).unwrap_or_else(|f| panic!("запуск: {f:?}"));
        assert_eq!(source.poll(0).status, FrameStatus::Starting);
        source.stop();
    }
}

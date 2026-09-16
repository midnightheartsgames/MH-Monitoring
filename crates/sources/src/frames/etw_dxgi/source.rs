//! Собственный ETW-потребитель как [`FrameSource`].

use std::sync::Arc;

use mh_core::{FpsReason, Millis};

use crate::clock::Clock;
use crate::frames::source::{
    Failure, FrameReport, FrameSource, FrameSourceFactory, FrameStatus, SourceKind,
};

use super::session::{EtwFrameSource, EtwStartError, EtwStatus};

/// Запускает собственный ETW-потребитель для очередной цели.
pub struct EtwFactory {
    /// Имя сессии — одно на запуск приложения, то же, что у PresentMon (PLAN.md §2.1.1). Оба
    /// источника никогда не работают одновременно: откат сначала останавливает основной.
    pub session_name: String,
    pub clock: Arc<dyn Clock>,
}

impl FrameSourceFactory for EtwFactory {
    fn kind(&self) -> SourceKind {
        SourceKind::OwnEtw
    }

    fn start(
        &self,
        target_process_id: u32,
        _session_id: u64,
    ) -> Result<Box<dyn FrameSource>, Failure> {
        let source = EtwFrameSource::start(
            &self.session_name,
            target_process_id,
            mh_platform::qpc_frequency(),
            Arc::clone(&self.clock),
        )
        .map_err(|error| match error {
            // Свой потребитель **не убирает** требование прав — ровно те же права, что у
            // PresentMon (PLAN.md §2.3). Отдельный код, чтобы откат на него не предлагали зря.
            EtwStartError::AccessDenied => Failure::new(FpsReason::NotPermitted),
            other => Failure::with_detail(FpsReason::BackendFailed, other.to_string()),
        })?;
        Ok(Box::new(EtwSource { inner: source }))
    }
}

/// Обёртка, а не реализация трейта прямо на [`EtwFrameSource`]: у того уже есть собственный
/// `stop() -> bool`, и два одноимённых метода на одном типе читались бы как ловушка.
struct EtwSource {
    inner: EtwFrameSource,
}

impl FrameSource for EtwSource {
    fn kind(&self) -> SourceKind {
        SourceKind::OwnEtw
    }

    fn frametimes(&self) -> &[f32] {
        self.inner.frametimes()
    }

    fn poll(&mut self, now_ms: Millis) -> FrameReport {
        let report = self.inner.poll(now_ms);
        let status = match report.status {
            EtwStatus::Starting => FrameStatus::Starting,
            EtwStatus::Measuring => FrameStatus::Measuring,
            EtwStatus::Stalled => FrameStatus::Stalled,
            EtwStatus::NoFrames => FrameStatus::NoFrames,
            EtwStatus::Ended => FrameStatus::Ended,
        };
        let counters = report.counters;
        FrameReport {
            status,
            statistics: report.statistics,
            last_frame_at_ms: report.last_frame_at_ms,
            diagnostics: format!(
                "Present {}, кадров {}, тестовых {}, помеченных-кадров {}, чужих цепочек {}, \
                 битых {}, потеряно событий {}",
                counters.presents,
                counters.frames,
                counters.test_presents,
                counters.flagged_frames,
                counters.other_chain,
                counters.malformed,
                report.events_lost
            ),
        }
    }

    fn stop(&mut self) {
        self.inner.stop();
    }
}

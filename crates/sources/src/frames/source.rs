//! Общий интерфейс источников кадров.
//!
//! Трейт написан **после** обеих реализаций, а не до: абстракция, придуманная заранее, получается
//! неправильной — это же правило PLAN.md формулирует для IPC в решении D2. Поэтому здесь ровно
//! то, что у PresentMon и собственного ETW-потребителя оказалось общим, и ничего сверх.

use mh_core::{FpsReason, FrameStatistics, Millis};

/// Какой источник даёт кадры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Вложенный PresentMon — основной источник (решение D1).
    PresentMon,
    /// Собственный ETW-потребитель DXGI/D3D9 — запасной.
    OwnEtw,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::PresentMon => "PresentMon",
            SourceKind::OwnEtw => "собственный ETW",
        }
    }

    /// Чего этот источник не видит, — пользователь обязан это знать, когда на нём сидит.
    ///
    /// Для собственного ETW это **OpenGL на весь экран**. Vulkan в оконном и безрамочном режиме
    /// на NVIDIA виден через DXGI (`spikes/etw-frames/COVERAGE.md` §4), OpenGL в окне — через
    /// событие вывода ядра (PLAN.md §2.16). OpenGL на весь экран этим путём не проверен.
    pub fn coverage_gap(self) -> Option<&'static str> {
        match self {
            SourceKind::PresentMon => None,
            SourceKind::OwnEtw => Some("OpenGL на весь экран может быть не виден"),
        }
    }
}

/// Диагноз: код причины и, если есть, слова источника.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: FpsReason,
    pub detail: Option<String>,
}

impl Failure {
    pub fn new(reason: FpsReason) -> Self {
        Self { reason, detail: None }
    }

    pub fn with_detail(reason: FpsReason, detail: impl Into<String>) -> Self {
        Self { reason, detail: Some(detail.into()) }
    }
}

/// Состояние сеанса захвата, общее для обоих источников.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameStatus {
    /// Захват идёт, первого кадра ещё не было, watchdog старта не истёк.
    Starting,
    Measuring,
    /// Кадры шли и прекратились.
    Stalled,
    /// Watchdog старта истёк: кадров не было вовсе.
    NoFrames,
    Failed(Failure),
    /// Источник завершился штатно — обычно вместе с игрой.
    Ended,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameReport {
    pub status: FrameStatus,
    pub statistics: FrameStatistics,
    pub last_frame_at_ms: Option<Millis>,
    /// Счётчики источника одной строкой: сколько пришло, сколько отброшено и почему.
    ///
    /// Нужна ровно тогда, когда кадров нет. Без неё «ноль кадров» не отличить от «кадры
    /// приходят и все до одного отбрасываются» — именно так фильтр тестовых Present, ошибочно
    /// применённый к D3D9, скрыл всю Devil May Cry 4 SE и был найден лишь рассуждением.
    pub diagnostics: String,
    /// Рантайм и режим вывода последних кадров, если источник их знает.
    pub presentation: Option<String>,
}

/// Живой сеанс захвата кадров одного процесса.
pub trait FrameSource: Send {
    fn kind(&self) -> SourceKind;
    /// Опрос без блокировки.
    fn poll(&mut self, now_ms: Millis) -> FrameReport;
    /// Кадры окна на момент последнего [`FrameSource::poll`], старейший первым. Для графика.
    fn frametimes(&self) -> &[f32] {
        &[]
    }
    /// Остановка. Обязана оставить систему без ETW-сессии и без живого дочернего процесса.
    fn stop(&mut self);
}

/// Чем запускать источник для очередной цели.
pub trait FrameSourceFactory: Send + Sync {
    fn kind(&self) -> SourceKind;
    fn start(
        &self,
        target_process_id: u32,
        session_id: u64,
    ) -> Result<Box<dyn FrameSource>, Failure>;
}

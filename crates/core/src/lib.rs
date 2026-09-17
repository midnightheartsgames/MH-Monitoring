//! Чистая логика MH Monitoring (PLAN.md §6/P1).
//!
//! В этом крейте нет ни одного системного вызова, ни одной зависимости и ни одного
//! `#[cfg(windows)]`. Он собирается и проходит тесты где угодно — это не украшение, а способ
//! держать всю содержательную логику под тестами, не заводя Windows и не имея прав на ETW.
//!
//! Время везде — монотонные миллисекунды ([`Millis`]), которые передаёт вызывающий. Так политика
//! debounce и TTL проверяется за микросекунды вместо реального ожидания, и ни одна из них не
//! зависит от перевода системных часов.
//!
//! Что здесь лежит:
//!
//! * [`statistics`] — средний FPS, 1 % и 0.1 % low по окну frametime;
//! * [`ring`] — ограниченная история frametime, по времени и по ёмкости;
//! * [`fps_state`] — одно состояние источника кадров: доступность, причина, цель, цифры;
//! * [`target`] — политика выбора измеряемого процесса;
//! * [`session_name`] — имена ETW-сессий и правило их уборки;
//! * [`telemetry`] — модель показаний железа и итоговый [`telemetry::Snapshot`];
//! * [`aggregator`] — слияние тиров опроса с TTL.

#![forbid(unsafe_code)]

pub mod aggregator;
pub mod fps_state;
pub mod graph;
pub mod ring;
pub mod session_name;
pub mod statistics;
pub mod target;
pub mod telemetry;

/// Монотонные миллисекунды.
///
/// Именно монотонные: TTL и debounce, посчитанные на системных часах, ломаются при переводе
/// времени и при синхронизации с NTP. Источник задаёт вызывающий, крейт часов не знает.
pub type Millis = u64;

pub use aggregator::{Aggregator, HardwareSample, SampleTier};
pub use fps_state::{FpsAvailability, FpsReason, FpsState, TargetProcess};
pub use graph::{FrametimeGraph, GraphBuilder};
pub use ring::FrametimeRing;
pub use session_name::{SESSION_PREFIX, session_name, should_sweep};
pub use statistics::FrameStatistics;
pub use target::{
    ProcessLookup, TargetMode, TargetResolution, TargetSettings, TargetTracker, is_shell_process,
};
pub use telemetry::{
    CpuStats, GpuStats, MemoryStats, SectionHealth, SensorReason, SensorStatus, Snapshot,
};

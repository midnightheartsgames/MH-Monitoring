//! Движок: владеет опросом источников и публикует [`mh_core::Snapshot`] (PLAN.md §4).
//!
//! UI ничего не знает об источниках — он берёт снимок через [`Engine::snapshot`] и получает
//! сигнал, когда снимок обновился. В P6 этот же движок уедет в службу, а UI будет получать снимки
//! по IPC; поэтому наружу торчит только снимок и настройки цели.

#[cfg(windows)]
mod bundle;
#[cfg(windows)]
mod engine;

#[cfg(windows)]
pub use bundle::extract_presentmon;
#[cfg(windows)]
pub use engine::{Engine, EngineConfig};

//! ETW: сессия, потребитель, гигиена.
//!
//! Порядок работы, нарушение которого проверено на опыте в P0:
//!
//! 1. подмести сирот ([`hygiene::sweep_orphans`]) — иначе чужая брошенная сессия обнулит
//!    захват, и искать причину вы будете в своём коде;
//! 2. создать сессию ([`session::Session::start`]);
//! 3. включить провайдеров — **до** открытия потребителя;
//! 4. открыть потребителя ([`consumer::Consumer::open`]) и крутить `process` на своём потоке;
//! 5. остановить сессию на любом пути выхода — это же разблокирует потребителя.

pub mod consumer;
pub mod hygiene;
pub mod session;

pub use consumer::{Consumer, EventInfo, EventSink, ProcessStatus};
pub use hygiene::{Sweep, stop_by_name, sweep_orphans};
pub use session::{Provider, Session, StartError, TraceStats, install_panic_cleanup};

//! Имена ETW-сессий и правило их уборки.
//!
//! Модуль крошечный, но живёт в `core` намеренно: **имя, под которым сессия создаётся, и
//! правило, по которому она подметается, обязаны быть определены в одном месте**. Когда они
//! разъезжаются, получается ровно то, что уже случилось на этом проекте: приложение создавало
//! `MHMonitor-<pid>`, спайк подметал только `MHMonitorSpike-*`, сирота от убитого приложения
//! пережила всё и обнулила захват кадров на всей машине. Измерено, см.
//! `spikes/etw-frames/COVERAGE.md` §1.
//!
//! Симптом сироты стоит помнить: постоянные потери событий, **не зависящие ни от нагрузки, ни
//! от настроек сессии**. Если потери одинаковы при 10 и при 120 кадрах в секунду — ищите не
//! в своём коде.

/// Префикс имени, под которым создаётся сессия приложения.
pub const SESSION_PREFIX: &str = "MHMonitor-";

/// Префикс уборки. **Шире, чем [`SESSION_PREFIX`], и без дефиса — это не опечатка.**
///
/// Сессии спайка называются `MHMonitorSpike-<pid>`, и они не начинаются с `MHMonitor-`: дефис
/// стоит в другом месте. Уборка по `MHMonitor-` прошла бы мимо них — ровно так спайк и не
/// подметал собственных сирот, пока это не поймал тест ниже.
///
/// Дальше расширять нельзя: `PresentMon` — общее имя сторонних инструментов (PLAN.md §2.1.2).
pub const SWEEP_PREFIX: &str = "MHMonitor";

/// Имя, под которым приложение создаёт свою сессию.
///
/// **Одно на запуск процесса, а не на захват.** Уникальное имя на каждую смену цели оставляло
/// бы по сироте на каждое переключение (PLAN.md §2.1.1).
pub fn session_name(process_id: u32) -> String {
    format!("{SESSION_PREFIX}{process_id}")
}

/// Надо ли остановить чужую сессию с таким именем.
///
/// `keep` — имя собственной сессии, которую трогать нельзя.
///
/// Правило намеренно узкое. Расширять его до «останавливать всё, что мешает» нельзя: имя
/// `PresentMon` общее для сторонних инструментов, и остановить его — значит сломать чужой
/// захват посреди чужого замера (PLAN.md §2.1.2).
pub fn should_sweep(name: &str, keep: &str) -> bool {
    name.starts_with(SWEEP_PREFIX) && name != keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_carries_the_prefix_and_the_pid() {
        assert_eq!(session_name(31_504), "MHMonitor-31504");
        assert!(session_name(1).starts_with(SESSION_PREFIX));
    }

    /// Имя по умолчанию `PresentMon` общее для чужих инструментов — своим оно быть не может.
    #[test]
    fn our_name_is_never_the_shared_default() {
        assert!(!session_name(42).eq_ignore_ascii_case("presentmon"));
    }

    #[test]
    fn our_own_session_is_never_swept() {
        let mine = session_name(100);
        assert!(!should_sweep(&mine, &mine));
    }

    #[test]
    fn an_orphan_of_the_application_is_swept() {
        assert!(should_sweep("MHMonitor-29244", &session_name(100)));
    }

    /// Имя спайка не начинается с `MHMonitor-`, и узкий префикс проходил мимо него.
    #[test]
    fn a_spike_orphan_is_swept_too() {
        assert!(!"MHMonitorSpike-27916".starts_with(SESSION_PREFIX), "дефис стоит не там");
        assert!(should_sweep("MHMonitorSpike-27916", &session_name(100)));
    }

    /// Граница, за которую уборка выходить не должна ни при каких обстоятельствах.
    #[test]
    fn foreign_sessions_are_never_touched() {
        for foreign in [
            "PresentMon",
            "presentmon",
            "NVIDIA FrameView",
            "Circular Kernel Context Logger",
            "EventLog-Application",
            // Похоже на наше, но не наше: префикс обязан совпадать с начала строки.
            "OtherMHMonitor-1",
            " MHMonitor-1",
        ] {
            assert!(!should_sweep(foreign, &session_name(100)), "нельзя трогать «{foreign}»");
        }
    }
}

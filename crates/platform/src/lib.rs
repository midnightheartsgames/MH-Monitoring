//! Тонкие обёртки Win32 (PLAN.md §4).
//!
//! Правило крейта: **минимум логики**. Всё, что можно решить без системных вызовов, решается в
//! `mh-core` и покрывается тестами, которые идут где угодно. Здесь остаётся только то, что без
//! Windows не выразить, — и остаётся настолько тонким, насколько выйдет. Скажем, правило уборки
//! ETW-сессий живёт в [`mh_core::session_name`], а этот крейт лишь перечисляет сессии и
//! останавливает те, которые оно назвало.
//!
//! На платформах, отличных от Windows, крейт собирается пустым. Это не поддержка других систем,
//! а способ сохранить осмысленной проверку всего рабочего пространства чужим таргетом:
//!
//! ```powershell
//! cargo check --workspace --target x86_64-unknown-linux-gnu
//! ```

#[cfg(windows)]
pub mod console;
#[cfg(windows)]
pub mod diag;
#[cfg(windows)]
pub mod dialog;
#[cfg(windows)]
pub mod elevate;
#[cfg(windows)]
pub mod etw;
#[cfg(windows)]
pub mod game_window;
#[cfg(windows)]
pub mod gpu;
#[cfg(windows)]
pub mod instance;
#[cfg(windows)]
pub mod job;
#[cfg(windows)]
pub mod overlay;
#[cfg(windows)]
pub mod pawnio;
#[cfg(windows)]
pub mod pdh;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod pump;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod shortcut;
#[cfg(windows)]
mod sys;
#[cfg(windows)]
pub mod system_info;

/// Частота `QueryPerformanceCounter`.
///
/// ETW-сессии создаются с `Wnode.ClientContext = 1`, и таймстемпы событий приходят в единицах
/// QPC. Дельта между ними переводится в миллисекунды именно по этой частоте.
#[cfg(windows)]
pub fn qpc_frequency() -> i64 {
    let mut frequency = 0i64;
    let ok = unsafe {
        windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency)
    };
    // На всех системах начиная с XP функция не падает. Запасное значение — лишь чтобы ошибка
    // никогда не превратилась в деление на ноль на каждом кадре.
    if ok == 0 || frequency <= 0 { 10_000_000 } else { frequency }
}

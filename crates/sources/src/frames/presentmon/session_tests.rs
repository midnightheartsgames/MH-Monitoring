//! Тесты жизненного цикла сеанса захвата (PLAN.md §8).
//!
//! Все сценарии из требований закрыты подставным ребёнком, без запуска настоящего PresentMon и
//! без прав администратора: молчащий ребёнок, только stderr, отказ в доступе, неподдерживаемая
//! схема, полсотни смен цели без утечки процессов и потоков.

use super::*;
use crate::clock::ManualClock;
use crate::frames::presentmon::child::fake::{FakeLauncher, Script, utf16le};
use crate::frames::presentmon::command::{self, ExecutableChoice};
use mh_core::FpsReason;
use std::path::PathBuf;

const HEADER: &str = "Application,ProcessID,SwapChainAddress,MsBetweenPresents";

fn command() -> CaptureCommand {
    let exe = ExecutableChoice::Bundled(PathBuf::from("PresentMon.exe"));
    command::build(&exe, &command::session_name(1234), 4242)
}

/// Заголовок и `count` строк одной цепочки обмена.
fn rows(count: usize, frametime_ms: f64) -> String {
    let mut text = format!("{HEADER}\n");
    for _ in 0..count {
        text.push_str(&format!("game.exe,4242,0xAAAA,{frametime_ms}\n"));
    }
    text
}

/// Ждёт, пока поток чтения дойдёт до нужного состояния.
///
/// Опрос в цикле, а не `sleep` наугад: читатель работает на своём потоке, и угадывать, сколько
/// ему нужно, — верный способ получить тест, который иногда падает на чужой машине.
fn wait_until(
    session: &mut CaptureSession,
    now_ms: Millis,
    mut ready: impl FnMut(&SessionReport) -> bool,
) -> SessionReport {
    for _ in 0..2_000 {
        let report = session.poll(now_ms);
        if ready(&report) {
            return report;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("поток чтения не дошёл до нужного состояния за отведённое время");
}

fn start(launcher: &FakeLauncher, clock: Arc<dyn Clock>) -> CaptureSession {
    CaptureSession::start(launcher, &command(), clock, Some(4242), 1).expect("подставной запуск")
}

// --- нормальный ход -----------------------------------------------------------------------

#[test]
fn frames_from_a_healthy_child_are_counted_and_measured() {
    let clock = Arc::new(ManualClock::new(1_000));
    let launcher = FakeLauncher::new(Script::stdout_text(&rows(120, 16.0)));
    let mut session = start(&launcher, clock);

    let report = wait_until(&mut session, 1_000, |r| r.counters.parsed >= 120);
    assert_eq!(report.status, SessionStatus::Measuring);
    assert_eq!(report.counters.rows, 120, "заголовок в строки данных не попадает");
    assert_eq!(report.counters.parsed, 120);
    assert_eq!(report.counters.rejected, 0);
    assert_eq!(report.swap_chain.as_deref(), Some("0xAAAA"));
    assert!((report.statistics.average_fps.unwrap() - 62.5).abs() < 0.1);
    session.stop();
}

/// §2.7: заголовок приходит сразу, кадры могут не прийти никогда.
#[test]
fn a_header_alone_is_not_a_measurement() {
    let clock = Arc::new(ManualClock::new(0));
    let launcher = FakeLauncher::new(Script::stdout_text(&format!("{HEADER}\n")));
    let mut session = start(&launcher, clock);

    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(session.poll(0).status, SessionStatus::Starting, "заголовок — не кадры");
    assert_eq!(session.poll(0).counters.rows, 0);
    session.stop();
}

// --- watchdog -----------------------------------------------------------------------------

/// Молчащий ребёнок: труба открыта, данных нет. Читатель на этом заблокирован — и именно
/// поэтому он живёт на отдельном потоке (§2.8). Управление обязано оставаться свободным.
#[test]
fn a_silent_child_trips_the_start_watchdog() {
    let clock = Arc::new(ManualClock::new(0));
    let launcher = FakeLauncher::new(Script::silent());
    let mut session = start(&launcher, clock);

    assert_eq!(session.poll(0).status, SessionStatus::Starting);
    assert_eq!(session.poll(FIRST_FRAME_TIMEOUT_MS - 1).status, SessionStatus::Starting);
    assert_eq!(session.poll(FIRST_FRAME_TIMEOUT_MS).status, SessionStatus::NoFrames);

    assert!(session.stop(), "после остановки ребёнка читатели обязаны завершиться");
}

#[test]
fn an_interrupted_stream_is_reported_sooner_than_a_cold_start() {
    let clock = Arc::new(ManualClock::new(1_000));
    let launcher = FakeLauncher::new(Script::stdout_text(&rows(10, 16.0)));
    let mut session = start(&launcher, clock);

    wait_until(&mut session, 1_000, |r| r.counters.parsed >= 10);
    assert_eq!(session.poll(1_000).status, SessionStatus::Measuring);
    assert_eq!(session.poll(1_000 + STALL_TIMEOUT_MS - 1).status, SessionStatus::Measuring);
    assert_eq!(session.poll(1_000 + STALL_TIMEOUT_MS).status, SessionStatus::Stalled);
    session.stop();
}

// --- отказы -------------------------------------------------------------------------------

/// Запуск без прав: 2.5.1 выходит с кодом 6 и пишет ошибку в stderr. Предупреждение про
/// privilege идёт первым и диагноз испортить не должно (§2.6).
#[test]
fn access_denied_is_diagnosed_as_missing_rights() {
    let clock = Arc::new(ManualClock::new(0));
    let launcher = FakeLauncher::new(Script::stderr_only(
        "warning: PresentMon requires elevated privilege in order to query processes\n\
         error: failed to start trace session: access denied.\n",
        6,
    ));
    let mut session = start(&launcher, clock);

    let report = wait_until(&mut session, 0, |r| r.status.is_terminal());
    match report.status {
        SessionStatus::Failed(failure) => assert_eq!(failure.reason, FpsReason::NotPermitted),
        other => panic!("ожидался отказ в правах, получено {other:?}"),
    }
    session.stop();
}

/// Ребёнок не пишет в stdout вовсе: только stderr и код возврата.
#[test]
fn a_child_that_only_writes_to_stderr_still_yields_a_diagnosis() {
    let clock = Arc::new(ManualClock::new(0));
    let launcher = FakeLauncher::new(Script::stderr_only("error: something new\n", 3));
    let mut session = start(&launcher, clock);

    let report = wait_until(&mut session, 0, |r| r.status.is_terminal());
    match report.status {
        SessionStatus::Failed(failure) => {
            assert_eq!(failure.reason, FpsReason::BackendFailed);
            assert_eq!(failure.detail.as_deref(), Some("something new"));
        }
        other => panic!("ожидался отказ, получено {other:?}"),
    }
    session.stop();
}

/// Заголовок без пригодной колонки frametime — эта сборка его прочитать не может.
#[test]
fn an_unsupported_header_fails_the_session_with_its_own_reason() {
    let clock = Arc::new(ManualClock::new(0));
    let launcher =
        FakeLauncher::new(Script::stdout_text("Application,ProcessID,PresentMode\nfoo,1,x\n"));
    let mut session = start(&launcher, clock);

    let report = wait_until(&mut session, 0, |r| r.status.is_terminal());
    match report.status {
        SessionStatus::Failed(failure) => {
            assert_eq!(failure.reason, FpsReason::UnsupportedCsv);
            assert!(
                failure.detail.unwrap().contains("PresentMode"),
                "заголовок попадает в диагноз"
            );
        }
        other => panic!("ожидалась неподдерживаемая схема, получено {other:?}"),
    }
    session.stop();
}

#[test]
fn a_launcher_that_cannot_start_the_exe_reports_the_io_error() {
    let clock = Arc::new(ManualClock::new(0)) as Arc<dyn Clock>;
    let launcher = FakeLauncher::failing(std::io::ErrorKind::NotFound);
    // `expect_err` здесь не годится: у живого сеанса нет и не должно быть `Debug` — он владеет
    // дочерним процессом и потоками.
    match CaptureSession::start(&launcher, &command(), clock, Some(4242), 1) {
        Ok(_) => panic!("запуск обязан провалиться"),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
}

/// Ребёнок ушёл вместе с игрой — это конец сеанса, а не поломка.
#[test]
fn a_clean_exit_ends_the_session_without_a_failure() {
    let clock = Arc::new(ManualClock::new(0));
    let launcher = FakeLauncher::new(Script {
        stdout: vec![utf16le(&rows(5, 16.0))],
        exit_code: Some(0),
        ..Default::default()
    });
    let mut session = start(&launcher, clock);

    let report = wait_until(&mut session, 0, |r| r.status.is_terminal());
    assert_eq!(report.status, SessionStatus::Ended { exit_code: Some(0) });
    session.stop();
}

// --- фильтрация ---------------------------------------------------------------------------

#[test]
fn rows_of_another_process_are_filtered_not_rejected() {
    let clock = Arc::new(ManualClock::new(0));
    let text = format!("{HEADER}\nother.exe,777,0xBBBB,16.0\ngame.exe,4242,0xAAAA,16.0\n");
    let launcher = FakeLauncher::new(Script::stdout_text(&text));
    let mut session = start(&launcher, clock);

    let report = wait_until(&mut session, 0, |r| r.counters.rows >= 2);
    assert_eq!(report.counters.filtered, 1);
    assert_eq!(report.counters.parsed, 1);
    assert_eq!(report.counters.rejected, 0);
    session.stop();
}

/// `NA` в колонке frametime — штатное значение PresentMon, но кадром такая строка не станет.
#[test]
fn unusable_rows_land_in_the_rejected_counter() {
    let clock = Arc::new(ManualClock::new(0));
    let text = format!("{HEADER}\ngame.exe,4242,0xAAAA,NA\ngame.exe,4242,0xAAAA,16.0\n");
    let launcher = FakeLauncher::new(Script::stdout_text(&text));
    let mut session = start(&launcher, clock);

    let report = wait_until(&mut session, 0, |r| r.counters.rows >= 2);
    assert_eq!(report.counters.rejected, 1);
    assert_eq!(report.counters.parsed, 1);
    session.stop();
}

// --- жизненный цикл -----------------------------------------------------------------------

/// Требование §8: полсотни смен цели без утечки процессов и потоков.
#[test]
fn fifty_target_switches_leak_neither_children_nor_readers() {
    let clock = Arc::new(ManualClock::new(0)) as Arc<dyn Clock>;
    let launcher = FakeLauncher::new(Script::silent());

    for session_id in 0..50 {
        let mut session = CaptureSession::start(
            &launcher,
            &command(),
            Arc::clone(&clock),
            Some(4242),
            session_id,
        )
        .expect("запуск");
        session.poll(0);
        assert!(session.stop(), "сеанс {session_id}: читатели не завершились");
    }

    assert_eq!(launcher.launches(), 50);
    assert_eq!(launcher.killed(), 50, "каждый ребёнок обязан быть завершён");
}

/// Даже если `stop` забыли, живого ребёнка после сеанса остаться не должно: он держит
/// ETW-сессию, а брошенная сессия ломает захват на всей машине (§2.1).
#[test]
fn dropping_a_session_still_kills_the_child() {
    let clock = Arc::new(ManualClock::new(0)) as Arc<dyn Clock>;
    let launcher = FakeLauncher::new(Script::silent());
    {
        let mut session =
            CaptureSession::start(&launcher, &command(), clock, Some(4242), 7).expect("запуск");
        session.poll(0);
    }
    assert_eq!(launcher.killed(), 1, "Drop обязан завершить ребёнка");
}

#[test]
fn stopping_twice_is_harmless() {
    let clock = Arc::new(ManualClock::new(0)) as Arc<dyn Clock>;
    let launcher = FakeLauncher::new(Script::silent());
    let mut session =
        CaptureSession::start(&launcher, &command(), clock, Some(4242), 1).expect("запуск");
    assert!(session.stop());
    session.stop();
}

/// Терминальный статус не должен «отыгрываться» обратно при следующем опросе.
#[test]
fn a_terminal_status_is_sticky() {
    let clock = Arc::new(ManualClock::new(0));
    let launcher = FakeLauncher::new(Script::stderr_only("error: access denied\n", 6));
    let mut session = start(&launcher, clock);

    let first = wait_until(&mut session, 0, |r| r.status.is_terminal()).status;
    assert_eq!(session.poll(100_000).status, first);
    session.stop();
}

// --- режим вывода, который PresentMon не отслеживает (§2.16) ------------------------------

const MODE_HEADER: &str = "Application,ProcessID,SwapChainAddress,PresentMode,MsBetweenPresents";

fn rows_with_mode(count: usize, mode: &str) -> String {
    let mut text = format!("{MODE_HEADER}\n");
    for _ in 0..count {
        text.push_str(&format!("dmc4.exe,4242,0xAAAA,{mode},16.0\n"));
    }
    text
}

#[test]
fn a_gdi_copy_stream_fails_with_its_own_reason() {
    let clock = Arc::new(ManualClock::new(1_000));
    let launcher =
        FakeLauncher::new(Script::stdout_text(&rows_with_mode(40, "Composed: Copy with GPU GDI")));
    let mut session = start(&launcher, clock);

    let report = wait_until(&mut session, 1_000, |r| r.status.is_terminal());
    match report.status {
        SessionStatus::Failed(failure) => {
            assert_eq!(failure.reason, FpsReason::PresentModeUntracked);
            assert!(failure.detail.unwrap().contains("Copy with GPU GDI"));
        }
        other => panic!("ожидался отказ, получено {other:?}"),
    }
}

#[test]
fn a_flip_stream_keeps_measuring() {
    let clock = Arc::new(ManualClock::new(1_000));
    let launcher = FakeLauncher::new(Script::stdout_text(&rows_with_mode(
        40,
        "Hardware Composed: Independent Flip",
    )));
    let mut session = start(&launcher, clock);
    let report = wait_until(&mut session, 1_000, |r| r.counters.parsed >= 40);
    assert_eq!(report.status, SessionStatus::Measuring);
    session.stop();
}

#[test]
fn recent_modes_need_a_warm_up_and_a_clear_majority() {
    let mut modes = RecentModes::default();
    for _ in 0..7 {
        modes.record(Some("Composed: Copy with GPU GDI"));
    }
    assert_eq!(modes.dominant_untracked(), None, "семи кадров мало для решения");
    modes.record(Some("Composed: Copy with GPU GDI"));
    assert_eq!(modes.dominant_untracked(), Some("Composed: Copy with GPU GDI"));

    // Переход на полный экран: свежие кадры — Independent Flip, старые вытесняются.
    for _ in 0..32 {
        modes.record(Some("Hardware Composed: Independent Flip"));
    }
    assert_eq!(modes.dominant_untracked(), None);

    // Половина на половину — не повод бросать основной источник.
    for index in 0..32 {
        let mode = if index % 2 == 0 { "Composed: Copy with GPU GDI" } else { "Composed: Flip" };
        modes.record(Some(mode));
    }
    assert_eq!(modes.dominant_untracked(), None);
}

#[test]
fn rows_without_a_mode_column_never_trigger() {
    let mut modes = RecentModes::default();
    for _ in 0..40 {
        modes.record(None);
    }
    assert_eq!(modes.dominant_untracked(), None);
}

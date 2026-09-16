//! Один сеанс захвата: дочерний процесс, два потока чтения и надзор над ними.
//!
//! Устройство продиктовано PLAN.md §2.8: **блокирующее чтение нельзя ставить на путь управления**.
//! `read` на трубе дочернего процесса неотменяем, и если читать его там же, где принимаются
//! решения, молчащая игра заблокирует смену цели и выход из приложения навсегда.
//!
//! Поэтому: чтение живёт на своих потоках, управляющий код только опрашивает состояние и
//! завершает сеанс. Завершение сеанса закрывает трубы — и этим разблокирует читателей.
//!
//! Заголовок CSV **не считается измерением** (§2.7). Он приходит сразу, а кадры могут не прийти
//! никогда; состояние «измеряем» наступает только после первого принятого кадра и дальше
//! держится watchdog'ом по таймеру, независимым от потока чтения.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use mh_core::{FrameStatistics, FrametimeRing, Millis};

use crate::clock::Clock;

use super::child::{CaptureChild, CaptureLauncher};
use super::command::CaptureCommand;
use super::csv::{RowRejection, Schema, parse_row};
use super::diagnosis::{
    BoundedTail, Failure, MAX_ERROR_LINES, classify_exit_code, classify_stderr,
};
use crate::frames::swapchain::SwapChainSelector;

/// Сколько ждать первый кадр, прежде чем признать, что их нет.
pub const FIRST_FRAME_TIMEOUT_MS: Millis = 5_000;
/// Сколько ждать продолжения уже идущего потока кадров.
///
/// Меньше, чем на старте: поток, который шёл и прервался, — это другая ситуация, и узнавать о
/// ней надо быстрее, чем о том, что захват вообще не начался.
pub const STALL_TIMEOUT_MS: Millis = 2_000;
/// Сколько ждать завершения потоков чтения после остановки ребёнка.
const READER_JOIN_TIMEOUT_MS: u64 = 2_000;

// Прервавшийся поток кадров обязан обнаруживаться быстрее, чем несостоявшийся старт. Проверка
// на этапе компиляции, а не в тесте: перепутать эти два числа местами — правка на одну строку,
// а последствие — статус, который приходит не вовремя.
const _: () = assert!(STALL_TIMEOUT_MS < FIRST_FRAME_TIMEOUT_MS);

const READ_CHUNK: usize = 8 * 1024;

/// Счётчики строк за сеанс.
///
/// Считаются, а не логируются: строка лога на каждый кадр — это сотни строк в секунду. Зато
/// соотношение принятых и отброшенных сразу показывает, что именно пошло не так.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    /// Строк CSV прочитано всего, не считая заголовка.
    pub rows: u64,
    /// Строк, ставших кадрами.
    pub parsed: u64,
    /// Строк, отбракованных как негодные: не число, не в диапазоне, слишком коротка.
    pub rejected: u64,
    /// Строк, отфильтрованных как чужие: другой процесс или другая цепочка обмена.
    pub filtered: u64,
}

#[derive(Debug, Default)]
struct AtomicCounters {
    rows: AtomicU64,
    parsed: AtomicU64,
    rejected: AtomicU64,
    filtered: AtomicU64,
}

impl AtomicCounters {
    fn snapshot(&self) -> Counters {
        Counters {
            rows: self.rows.load(Ordering::Relaxed),
            parsed: self.parsed.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            filtered: self.filtered.load(Ordering::Relaxed),
        }
    }
}

/// Окно кадров под одной блокировкой.
///
/// Всё, что поток чтения пишет на каждый кадр, лежит вместе: один захват блокировки на кадр,
/// а не три.
#[derive(Debug)]
struct FrameWindow {
    ring: FrametimeRing,
    last_frame_at_ms: Option<Millis>,
    swap_chain: Option<String>,
}

#[derive(Debug)]
struct Shared {
    window: Mutex<FrameWindow>,
    counters: AtomicCounters,
    stderr_tail: Mutex<BoundedTail>,
    /// Заголовок, который эта сборка прочитать не смогла. Заполняется один раз.
    unsupported_header: Mutex<Option<String>>,
    schema_found: AtomicBool,
    /// Дочитаны ли трубы. Пока нет — код возврата ещё не вся правда о сеансе.
    stdout_finished: AtomicBool,
    stderr_finished: AtomicBool,
}

impl Shared {
    fn readers_finished(&self) -> bool {
        self.stdout_finished.load(Ordering::SeqCst) && self.stderr_finished.load(Ordering::SeqCst)
    }
}

/// Чем закончился опрос сеанса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// Захват запущен, первый кадр ещё не пришёл, watchdog старта не истёк.
    Starting,
    /// Кадры идут.
    Measuring,
    /// Кадры шли и прекратились.
    Stalled,
    /// Watchdog старта истёк: кадров не было вовсе.
    NoFrames,
    /// Сеанс непригоден, с диагнозом.
    Failed(Failure),
    /// Ребёнок завершился штатно — обычно вместе с игрой.
    Ended { exit_code: Option<i32> },
}

impl SessionStatus {
    pub fn is_measuring(&self) -> bool {
        matches!(self, SessionStatus::Measuring)
    }

    /// Сеанс закончился и продолжать его нельзя.
    pub fn is_terminal(&self) -> bool {
        matches!(self, SessionStatus::Failed(_) | SessionStatus::Ended { .. })
    }
}

/// Полный снимок состояния сеанса.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionReport {
    pub status: SessionStatus,
    pub statistics: FrameStatistics,
    pub counters: Counters,
    pub swap_chain: Option<String>,
    pub last_frame_at_ms: Option<Millis>,
    pub session_id: u64,
}

/// Живой сеанс захвата.
pub struct CaptureSession {
    shared: Arc<Shared>,
    child: Box<dyn CaptureChild>,
    stop: Arc<AtomicBool>,
    reader_done: mpsc::Receiver<()>,
    readers: usize,
    started_at_ms: Millis,
    session_id: u64,
    /// Кэш итога: после терминального статуса он не меняется.
    finished: Option<SessionStatus>,
    ordered: Vec<f32>,
    scratch: Vec<f32>,
}

impl CaptureSession {
    /// Запускает захват.
    ///
    /// `target_process_id` дублирует фильтрацию, которую делает сам PresentMon: колонка PID в
    /// CSV есть не во всех поколениях, а когда есть — лишняя проверка ничего не стоит.
    pub fn start(
        launcher: &dyn CaptureLauncher,
        command: &CaptureCommand,
        clock: Arc<dyn Clock>,
        target_process_id: Option<u32>,
        session_id: u64,
    ) -> std::io::Result<Self> {
        let mut child = launcher.launch(command)?;
        let started_at_ms = clock.now_ms();

        let shared = Arc::new(Shared {
            window: Mutex::new(FrameWindow {
                ring: FrametimeRing::new(),
                last_frame_at_ms: None,
                swap_chain: None,
            }),
            counters: AtomicCounters::default(),
            stderr_tail: Mutex::new(BoundedTail::new(MAX_ERROR_LINES)),
            unsupported_header: Mutex::new(None),
            schema_found: AtomicBool::new(false),
            // По умолчанию «дочитано»: если трубы нет, ждать от неё нечего.
            stdout_finished: AtomicBool::new(true),
            stderr_finished: AtomicBool::new(true),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (done_tx, reader_done) = mpsc::channel();
        let mut readers = 0;

        if let Some(stream) = child.take_stdout() {
            shared.stdout_finished.store(false, Ordering::SeqCst);
            let shared = Arc::clone(&shared);
            let clock = Arc::clone(&clock);
            let done = done_tx.clone();
            std::thread::Builder::new().name(format!("presentmon-stdout-{session_id}")).spawn(
                move || {
                    pump_stdout(&shared, stream, clock.as_ref(), target_process_id);
                    shared.stdout_finished.store(true, Ordering::SeqCst);
                    let _ = done.send(());
                },
            )?;
            readers += 1;
        }

        if let Some(stream) = child.take_stderr() {
            shared.stderr_finished.store(false, Ordering::SeqCst);
            let shared = Arc::clone(&shared);
            let done = done_tx.clone();
            std::thread::Builder::new().name(format!("presentmon-stderr-{session_id}")).spawn(
                move || {
                    pump_stderr(&shared, stream);
                    shared.stderr_finished.store(true, Ordering::SeqCst);
                    let _ = done.send(());
                },
            )?;
            readers += 1;
        }

        Ok(Self {
            shared,
            child,
            stop,
            reader_done,
            readers,
            started_at_ms,
            session_id,
            finished: None,
            ordered: Vec::new(),
            scratch: Vec::new(),
        })
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Опрашивает состояние сеанса. Не блокирует.
    pub fn poll(&mut self, now_ms: Millis) -> SessionReport {
        let counters = self.shared.counters.snapshot();
        let (statistics, swap_chain, last_frame_at_ms) = {
            let mut window = self.shared.window.lock().unwrap_or_else(|e| e.into_inner());
            let statistics = window.ring.statistics(now_ms, &mut self.ordered, &mut self.scratch);
            (statistics, window.swap_chain.clone(), window.last_frame_at_ms)
        };

        let status = self.status(now_ms, last_frame_at_ms);
        SessionReport {
            status,
            statistics,
            counters,
            swap_chain,
            last_frame_at_ms,
            session_id: self.session_id,
        }
    }

    fn status(&mut self, now_ms: Millis, last_frame_at_ms: Option<Millis>) -> SessionStatus {
        if let Some(finished) = &self.finished {
            return finished.clone();
        }

        // Неподдерживаемая схема — приговор сеансу независимо от всего остального: заголовок
        // прочитан, а понять его нечем, и кадров не будет.
        // Блокировки снимаются до `finish`, иначе он не сможет взять `self` изменяемо.
        let unsupported =
            self.shared.unsupported_header.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(header) = unsupported {
            let failure = Failure {
                reason: mh_core::FpsReason::UnsupportedCsv,
                detail: Some(shorten_header(&header)),
            };
            return self.finish(SessionStatus::Failed(failure));
        }

        // Текст stderr авторитетнее кода возврата и учитывается сразу, как только прочитан.
        let by_text = {
            let tail = self.shared.stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
            classify_stderr(&tail)
        };
        if let Some(failure) = by_text {
            return self.finish(SessionStatus::Failed(failure));
        }

        // А вот выводы по коду возврата откладываются, пока трубы не дочитаны до конца.
        // Ребёнок успевает умереть раньше, чем поток чтения разберёт его stderr, и поспешный
        // диагноз «завершился с кодом N» затёр бы точное сообщение об ошибке.
        let exit_code = self.child.try_exit_code().ok().flatten();
        if let Some(code) = exit_code
            && self.shared.readers_finished()
        {
            return match classify_exit_code(code) {
                Some(failure) => self.finish(SessionStatus::Failed(failure)),
                None => self.finish(SessionStatus::Ended { exit_code }),
            };
        }

        match last_frame_at_ms {
            // Watchdog старта. Заголовок мог прийти, но заголовок — не измерение (§2.7).
            None => {
                if now_ms.saturating_sub(self.started_at_ms) >= FIRST_FRAME_TIMEOUT_MS {
                    SessionStatus::NoFrames
                } else {
                    SessionStatus::Starting
                }
            }
            Some(at) => {
                if now_ms.saturating_sub(at) >= STALL_TIMEOUT_MS {
                    SessionStatus::Stalled
                } else {
                    SessionStatus::Measuring
                }
            }
        }
    }

    fn finish(&mut self, status: SessionStatus) -> SessionStatus {
        self.finished = Some(status.clone());
        status
    }

    /// Останавливает захват и дожидается потоков чтения.
    ///
    /// Порядок обязателен: сначала гасим ребёнка, и только это закрывает трубы и разблокирует
    /// читателей. Ждать их, не завершив ребёнка, значит ждать вечно (§2.8).
    ///
    /// Возвращает `false`, если хотя бы один читатель не завершился за отведённое время. Такой
    /// поток отцепляется; ребёнок при этом уже мёртв, и ETW-сессию гасит вызывающий по имени.
    pub fn stop(&mut self) -> bool {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.child.kill();

        let deadline = Duration::from_millis(READER_JOIN_TIMEOUT_MS);
        for _ in 0..self.readers {
            if self.reader_done.recv_timeout(deadline).is_err() {
                return false;
            }
        }
        true
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        // Ни один путь выхода не должен оставить живого ребёнка: он держит ETW-сессию.
        self.stop();
    }
}

fn shorten_header(header: &str) -> String {
    const MAX: usize = 120;
    if header.chars().count() <= MAX {
        return header.to_string();
    }
    header.chars().take(MAX - 1).chain(std::iter::once('…')).collect()
}

/// Поток чтения stdout: декодирует, разбирает и записывает кадры.
///
/// Разбор идёт здесь, а не в управляющем коде, намеренно: очередь строк между потоками при
/// 500 кадрах в секунду росла бы ровно настолько, насколько опаздывает опрос. Кольцевой буфер
/// ограничен по построению, очередь — нет.
fn pump_stdout(
    shared: &Shared,
    mut stream: Box<dyn Read + Send>,
    clock: &dyn Clock,
    target_process_id: Option<u32>,
) {
    let mut decoder = super::decode::LineDecoder::new();
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut lines = Vec::new();
    let mut schema: Option<Schema> = None;
    let mut swap_chains: SwapChainSelector<String> = SwapChainSelector::new();

    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        decoder.push(&buffer[..read], &mut lines);
        for line in lines.drain(..) {
            handle_line(shared, &line, &mut schema, &mut swap_chains, clock, target_process_id);
        }
    }

    decoder.finish(&mut lines);
    for line in lines.drain(..) {
        handle_line(shared, &line, &mut schema, &mut swap_chains, clock, target_process_id);
    }
}

fn handle_line(
    shared: &Shared,
    line: &str,
    schema: &mut Option<Schema>,
    swap_chains: &mut SwapChainSelector,
    clock: &dyn Clock,
    target_process_id: Option<u32>,
) {
    if line.trim().is_empty() {
        return;
    }

    if schema.is_none() {
        // Схема фиксируется один раз из заголовка и дальше не перепроверяется (§2.5).
        match Schema::parse(line, false) {
            Some(parsed) => {
                shared.schema_found.store(true, Ordering::SeqCst);
                *schema = Some(parsed);
            }
            None => {
                let mut slot = shared.unsupported_header.lock().unwrap_or_else(|e| e.into_inner());
                if slot.is_none() {
                    *slot = Some(line.to_string());
                }
            }
        }
        // Заголовок — не строка данных: разбирать его как кадр нельзя, иначе он осядет в
        // счётчике отбракованных и будет выглядеть как порча потока.
        return;
    }
    let schema = schema.as_ref().expect("схема установлена выше");

    shared.counters.rows.fetch_add(1, Ordering::Relaxed);
    let frame = match parse_row(schema, line, target_process_id) {
        Ok(frame) => frame,
        Err(RowRejection::OtherProcess | RowRejection::OtherSwapChain) => {
            shared.counters.filtered.fetch_add(1, Ordering::Relaxed);
            return;
        }
        Err(_) => {
            shared.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let now_ms = clock.now_ms();
    if let Some(address) = frame.swap_chain
        && !swap_chains.accept(address, now_ms)
    {
        shared.counters.filtered.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let mut window = shared.window.lock().unwrap_or_else(|e| e.into_inner());
    if window.ring.record(frame.frame_time_ms, now_ms) {
        window.last_frame_at_ms = Some(now_ms);
        if window.swap_chain.as_ref() != swap_chains.selected() {
            window.swap_chain = swap_chains.selected().cloned();
        }
        drop(window);
        shared.counters.parsed.fetch_add(1, Ordering::Relaxed);
    } else {
        drop(window);
        shared.counters.rejected.fetch_add(1, Ordering::Relaxed);
    }
}

/// Поток чтения stderr: копит ограниченный хвост.
fn pump_stderr(shared: &Shared, mut stream: Box<dyn Read + Send>) {
    let mut decoder = super::decode::LineDecoder::new();
    let mut buffer = vec![0u8; 4 * 1024];
    let mut lines = Vec::new();

    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        decoder.push(&buffer[..read], &mut lines);
        push_lines(shared, &mut lines);
    }
    decoder.finish(&mut lines);
    push_lines(shared, &mut lines);
}

fn push_lines(shared: &Shared, lines: &mut Vec<String>) {
    if lines.is_empty() {
        return;
    }
    let mut tail = shared.stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
    for line in lines.drain(..) {
        if !line.trim().is_empty() {
            tail.push(line);
        }
    }
}

// Тесты вынесены в отдельный файл: их больше, чем самой реализации, и вместе они читались бы
// хуже. `#[path]` оставляет их обычным дочерним модулем — со свободным доступом к приватным
// полям сеанса.
#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;

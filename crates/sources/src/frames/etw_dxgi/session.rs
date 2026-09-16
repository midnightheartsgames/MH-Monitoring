//! Сеанс собственного ETW-потребителя кадров.
//!
//! Порядок вызовов здесь — не стилистика, а результат отладки P0. Нарушение любого шага даёт
//! одну и ту же обманчивую картину: `OpenTrace` успешен, `ProcessTrace` честно блокирует поток
//! до остановки сессии и возвращает `ERROR_SUCCESS`, а событий нет ни одного.
//!
//! 1. подмести сирот — чужая брошенная сессия обнуляет захват на всей машине;
//! 2. создать сессию;
//! 3. **включить провайдеров до открытия потребителя**;
//! 4. открыть потребителя и крутить `process` на отдельном потоке (PLAN.md §2.8);
//! 5. остановить сессию — это же разблокирует потребителя.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use mh_core::{FrameStatistics, FrametimeRing, Millis};
use mh_platform::etw::{
    Consumer, EventInfo, EventSink, Provider, Session, StartError, sweep_orphans,
};

use crate::clock::Clock;

use super::payload::{Counters, FrameAccumulator, KERNEL_PRESENT_ID, ProviderKind};

/// `Microsoft-Windows-DXGI`. GUID и keywords сняты с дампа живой сессии PresentMon, а не из
/// документации (PLAN.md §2.9).
///
/// Уровень 4 вместо 255: у Present-событий в манифесте `level = 0` (LogAlways), они проходят при
/// любом уровне, а 255 лишь тянет в сессию вербозный поток со всей системы.
pub const PROVIDER_DXGI: Provider = Provider {
    label: "Microsoft-Windows-DXGI",
    guid: 0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9,
    level: 4,
    any_keyword: 0x8000_0000_0000_0002,
};

/// `Microsoft-Windows-D3D9`.
pub const PROVIDER_D3D9: Provider = Provider {
    label: "Microsoft-Windows-D3D9",
    guid: 0x783ACA0A_790E_4D7F_8451_AA850511C6B9,
    level: 4,
    any_keyword: 0x8000_0000_0000_0002,
};

/// `Microsoft-Windows-DxgKrnl` — только ради события вывода кадра ядром (OpenGL в окне).
///
/// Включается **с фильтром по номеру события**: без него даже на этих настройках идут десятки
/// тысяч событий в секунду, с ним — ровно одно на кадр (проверено пробником на Ion Fury).
pub const PROVIDER_DXGKRNL: Provider = Provider {
    label: "Microsoft-Windows-DxgKrnl",
    guid: 0x802EC45A_1E99_4B83_9920_87C98277BA9D,
    level: 4,
    any_keyword: 0x1,
};

/// Сколько ждать первый кадр, прежде чем признать, что их нет.
pub const FIRST_FRAME_TIMEOUT_MS: Millis = 5_000;
/// Сколько ждать продолжения уже идущего потока кадров.
pub const STALL_TIMEOUT_MS: Millis = 2_000;
const READER_JOIN_TIMEOUT_MS: u64 = 5_000;

const _: () = assert!(STALL_TIMEOUT_MS < FIRST_FRAME_TIMEOUT_MS);

/// Чем закончился опрос сеанса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EtwStatus {
    Starting,
    Measuring,
    Stalled,
    /// Кадров не было вовсе за отведённое время.
    NoFrames,
    /// Потребитель вернулся сам — обычно потому, что сессию кто-то остановил снаружи.
    Ended,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EtwReport {
    pub status: EtwStatus,
    pub statistics: FrameStatistics,
    pub counters: Counters,
    pub swap_chain: Option<u64>,
    pub chain_switches: u32,
    pub last_frame_at_ms: Option<Millis>,
    /// Сколько событий сессия потеряла по данным потребителя.
    ///
    /// Ненулевое значение при любых настройках — признак осиротевшей сессии в системе, а не
    /// поломки здесь (`spikes/etw-frames/COVERAGE.md` §1).
    pub events_lost: u32,
}

#[derive(Debug)]
struct FrameWindow {
    ring: FrametimeRing,
    last_frame_at_ms: Option<Millis>,
}

/// Приёмник событий: превращает Present'ы в frametime и складывает их в окно.
struct FrameSink {
    accumulator: Mutex<FrameAccumulator>,
    window: Mutex<FrameWindow>,
    events_lost: std::sync::atomic::AtomicU32,
    stop: Arc<AtomicBool>,
    target_process_id: u32,
    clock: Arc<dyn Clock>,
    dxgi_guid: u128,
    d3d9_guid: u128,
    kernel_guid: u128,
}

impl EventSink for FrameSink {
    fn on_event(&self, event: &EventInfo<'_>) {
        // Фильтр по PID обязателен: через DXGI презентят одновременно и `dwm.exe`, и игра, и
        // всё остальное, что рисует на экране. `dwm.exe` при этом самый активный из всех.
        if event.process_id != self.target_process_id {
            return;
        }
        let now_ms = self.clock.now_ms();
        let frametime_ms = if event.provider_guid == self.kernel_guid {
            if event.event_id != KERNEL_PRESENT_ID {
                return;
            }
            let mut accumulator = self.accumulator.lock().unwrap_or_else(|e| e.into_inner());
            accumulator.on_kernel_present(event.timestamp_qpc)
        } else {
            let provider = if event.provider_guid == self.dxgi_guid {
                ProviderKind::Dxgi
            } else if event.provider_guid == self.d3d9_guid {
                ProviderKind::D3d9
            } else {
                return;
            };
            if !provider.is_present_start(event.event_id) {
                return;
            }
            let mut accumulator = self.accumulator.lock().unwrap_or_else(|e| e.into_inner());
            accumulator.on_present_start(provider, event.timestamp_qpc, event.user_data, now_ms)
        };
        let Some(frametime_ms) = frametime_ms else { return };

        let mut window = self.window.lock().unwrap_or_else(|e| e.into_inner());
        if window.ring.record(frametime_ms, now_ms) {
            window.last_frame_at_ms = Some(now_ms);
        }
    }

    fn on_buffer(&self, _buffers_read: u32, events_lost: u32) -> bool {
        self.events_lost.fetch_max(events_lost, Ordering::Relaxed);
        !self.stop.load(Ordering::SeqCst)
    }
}

/// Живой сеанс собственного ETW-потребителя.
pub struct EtwFrameSource {
    sink: Arc<FrameSink>,
    session: Session,
    stop: Arc<AtomicBool>,
    reader_done: mpsc::Receiver<mh_platform::etw::consumer::ProcessStatus>,
    started_at_ms: Millis,
    ended: bool,
    /// Сообщение потока чтения уже получено. Второго не будет — ждать его нельзя.
    reader_finished: bool,
    /// Итог первой остановки. Повторная (явная, а потом из `Drop`) не ждёт заново: иначе
    /// каждая смена цели и каждый выход стояли бы полный таймаут.
    stopped: Option<bool>,
    ordered: Vec<f32>,
    scratch: Vec<f32>,
}

/// Почему сеанс не удалось начать.
#[derive(Debug)]
pub enum EtwStartError {
    /// Realtime ETW требует администратора либо группы `Performance Log Users` (PLAN.md §2.3).
    /// Собственный потребитель **не убирает** это требование.
    AccessDenied,
    Session(StartError),
    /// Ни один провайдер не включился — измерять нечем.
    NoProviders,
    Consumer(u32),
    Spawn(std::io::Error),
}

impl std::fmt::Display for EtwStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EtwStartError::AccessDenied => write!(
                formatter,
                "нет прав на realtime ETW: нужен администратор либо группа «Performance Log Users»"
            ),
            EtwStartError::Session(error) => write!(formatter, "сессия не создана: {error}"),
            EtwStartError::NoProviders => write!(formatter, "ни один провайдер не включён"),
            EtwStartError::Consumer(code) => write!(formatter, "потребитель не открыт: код {code}"),
            EtwStartError::Spawn(error) => write!(formatter, "поток чтения не запущен: {error}"),
        }
    }
}

impl EtwFrameSource {
    /// Запускает захват кадров процесса `target_process_id`.
    pub fn start(
        session_name: &str,
        target_process_id: u32,
        qpc_frequency: i64,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, EtwStartError> {
        // Шаг 1. Сирота в системе обнуляет захват независимо от всего остального.
        sweep_orphans(session_name);

        // Шаг 2.
        let session = match Session::start(session_name) {
            Ok(session) => session,
            Err(StartError::AccessDenied) => return Err(EtwStartError::AccessDenied),
            Err(other) => return Err(EtwStartError::Session(other)),
        };

        // Шаг 3. Именно до открытия потребителя.
        let enabled = [&PROVIDER_DXGI, &PROVIDER_D3D9]
            .into_iter()
            .filter(|provider| session.enable_provider(provider, None).is_ok())
            .count()
            + usize::from(
                session.enable_provider_filtered(&PROVIDER_DXGKRNL, &[KERNEL_PRESENT_ID]).is_ok(),
            );
        if enabled == 0 {
            return Err(EtwStartError::NoProviders);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(FrameSink {
            accumulator: Mutex::new(FrameAccumulator::new(qpc_frequency)),
            window: Mutex::new(FrameWindow { ring: FrametimeRing::new(), last_frame_at_ms: None }),
            events_lost: std::sync::atomic::AtomicU32::new(0),
            stop: Arc::clone(&stop),
            target_process_id,
            clock: Arc::clone(&clock),
            dxgi_guid: PROVIDER_DXGI.guid,
            d3d9_guid: PROVIDER_D3D9.guid,
            kernel_guid: PROVIDER_DXGKRNL.guid,
        });

        // Шаг 4.
        let consumer =
            Consumer::open_with_sink(session_name, Arc::clone(&sink) as Arc<dyn EventSink>)
                .map_err(EtwStartError::Consumer)?;

        let (done_tx, reader_done) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("etw-frames-{target_process_id}"))
            .spawn(move || {
                let status = consumer.process();
                let (buffers_read, events_lost) = consumer.counters();
                let _ = done_tx.send(mh_platform::etw::consumer::ProcessStatus {
                    status,
                    buffers_read,
                    events_lost,
                });
            })
            .map_err(EtwStartError::Spawn)?;

        let started_at_ms = clock.now_ms();
        Ok(Self {
            sink,
            session,
            stop,
            reader_done,
            started_at_ms,
            ended: false,
            reader_finished: false,
            stopped: None,
            ordered: Vec::new(),
            scratch: Vec::new(),
        })
    }

    pub fn session_name(&self) -> &str {
        self.session.name()
    }

    /// Кадры окна на момент последнего [`Self::poll`], старейший первым.
    pub fn frametimes(&self) -> &[f32] {
        &self.ordered
    }

    pub fn poll(&mut self, now_ms: Millis) -> EtwReport {
        // Поток чтения вернулся сам — значит сессию кто-то остановил снаружи.
        if !self.reader_finished && self.reader_done.try_recv().is_ok() {
            self.reader_finished = true;
            self.ended = true;
        }

        let counters = self.sink.accumulator.lock().unwrap_or_else(|e| e.into_inner()).counters();
        let (swap_chain, chain_switches) = {
            let accumulator = self.sink.accumulator.lock().unwrap_or_else(|e| e.into_inner());
            (accumulator.selected_chain(), accumulator.chain_switches())
        };
        let (statistics, last_frame_at_ms) = {
            let mut window = self.sink.window.lock().unwrap_or_else(|e| e.into_inner());
            let statistics = window.ring.statistics(now_ms, &mut self.ordered, &mut self.scratch);
            (statistics, window.last_frame_at_ms)
        };

        let status = if self.ended {
            EtwStatus::Ended
        } else {
            match last_frame_at_ms {
                None if now_ms.saturating_sub(self.started_at_ms) >= FIRST_FRAME_TIMEOUT_MS => {
                    EtwStatus::NoFrames
                }
                None => EtwStatus::Starting,
                Some(at) if now_ms.saturating_sub(at) >= STALL_TIMEOUT_MS => EtwStatus::Stalled,
                Some(_) => EtwStatus::Measuring,
            }
        };

        EtwReport {
            status,
            statistics,
            counters,
            swap_chain,
            chain_switches,
            last_frame_at_ms,
            events_lost: self.sink.events_lost.load(Ordering::Relaxed),
        }
    }

    /// Останавливает сеанс.
    ///
    /// Остановка сессии — единственное, что разблокирует `ProcessTrace`. Ждать поток чтения,
    /// не остановив сессию, значит ждать вечно (PLAN.md §2.8).
    pub fn stop(&mut self) -> bool {
        if let Some(result) = self.stopped {
            return result;
        }
        self.stop.store(true, Ordering::SeqCst);
        self.session.stop();
        let joined = self.reader_finished
            || self.reader_done.recv_timeout(Duration::from_millis(READER_JOIN_TIMEOUT_MS)).is_ok();
        self.reader_finished |= joined;
        self.stopped = Some(joined);
        joined
    }
}

impl Drop for EtwFrameSource {
    fn drop(&mut self) {
        // Брошенная realtime-сессия ломает захват кадров на всей машине — ни один путь выхода
        // не имеет права её оставить (PLAN.md §2.1).
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MonotonicClock;

    /// Запуск от обычного пользователя обязан честно провалиться отказом в правах, а с правами
    /// — подняться и остановиться, ничего за собой не оставив.
    #[test]
    fn a_session_either_starts_cleanly_or_says_it_has_no_rights() {
        let name = mh_core::session_name(std::process::id());
        let clock = MonotonicClock::shared();
        match EtwFrameSource::start(&name, std::process::id(), 10_000_000, clock) {
            Ok(mut source) => {
                assert_eq!(source.session_name(), name);
                let report = source.poll(0);
                assert_eq!(report.status, EtwStatus::Starting, "кадров сразу не бывает");
                assert_eq!(report.counters.frames, 0);
                assert!(source.stop(), "поток чтения обязан завершиться после остановки сессии");
            }
            Err(EtwStartError::AccessDenied) => {}
            Err(other) => panic!("неожиданная ошибка запуска: {other}"),
        }
    }

    #[test]
    fn the_providers_carry_the_guids_measured_in_p0() {
        assert_eq!(PROVIDER_DXGI.guid, 0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
        assert_eq!(PROVIDER_D3D9.guid, 0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
        // Уровень выбран сознательно ниже 255: у Present-событий в манифесте `level = 0`
        // (LogAlways), они проходят при любом уровне, а 255 тянет в сессию вербозный поток
        // со всей системы. Если кто-то вернёт 255, он сделает это не по недосмотру.
        assert_eq!(PROVIDER_DXGI.level, 4);
        assert_eq!(PROVIDER_D3D9.level, 4);
        assert_eq!(PROVIDER_DXGI.any_keyword, 0x8000_0000_0000_0002);
    }
}

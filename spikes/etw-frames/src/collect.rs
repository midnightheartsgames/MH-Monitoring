//! Сбор событий: гистограмма по (процесс, провайдер, событие) и frametime для цели.
//!
//! Всё, что делает callback, обязано быть без паник: он `extern "system"`, и паника там не
//! разворачивает стек, а убивает процесс — вместе с шансом остановить ETW-сессию.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use windows_sys::Win32::System::Diagnostics::Etw::{EVENT_RECORD, EVENT_TRACE_LOGFILEW};

use crate::etw::{PROVIDER_D3D9, PROVIDER_DXGI};

/// Идентификаторы Present-событий манифестных провайдеров.
///
/// Спайк не обязан им верить: гистограмма показывает все пришедшие ID, и если реальность
/// окажется другой, это будет видно в отчёте, а не спрятано за нулём кадров.
pub const DXGI_PRESENT_START_ID: u16 = 42;
pub const D3D9_PRESENT_START_ID: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ProviderKey(pub u128);

impl ProviderKey {
    fn from_guid(g: &windows_sys::core::GUID) -> Self {
        let hi = ((g.data1 as u128) << 96) | ((g.data2 as u128) << 80) | ((g.data3 as u128) << 64);
        ProviderKey(hi | u64::from_be_bytes(g.data4) as u128)
    }

    pub fn label(&self) -> String {
        if *self == ProviderKey::from_guid(&PROVIDER_DXGI.guid) {
            "DXGI".to_string()
        } else if *self == ProviderKey::from_guid(&PROVIDER_D3D9.guid) {
            "D3D9".to_string()
        } else {
            let v = self.0;
            format!(
                "{:08x}-{:04x}-{:04x}-{:016x}",
                (v >> 96) as u32,
                (v >> 80) as u16,
                (v >> 64) as u16,
                v as u64
            )
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventKey {
    pub pid: u32,
    pub provider: ProviderKey,
    pub id: u16,
    pub version: u8,
    pub opcode: u8,
}

/// Поток Present-событий одной цепочки обмена.
///
/// Цепочек у процесса бывает несколько (оверлеи, вторичные окна), и смешивать их в один
/// frametime нельзя — получится пила из двух независимых частот. Спайк разделяет их по
/// адресу swapchain и отчитывается по доминирующей.
#[derive(Default)]
pub struct Chain {
    pub provider: Option<ProviderKey>,
    pub presents: u64,
    pub frames: Vec<f32>,
    last_ts: Option<i64>,
    pub first_ts: Option<i64>,
    pub last_seen_ts: Option<i64>,
}

#[derive(Default)]
pub struct State {
    pub events: BTreeMap<EventKey, u64>,
    pub chains: BTreeMap<u64, Chain>,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    /// Present-события цели, у которых не удалось прочитать адрес swapchain.
    pub presents_without_chain: u64,
    /// Отброшенные тестовые Present.
    pub filtered_test_presents: u64,
    /// Какие флаги вообще встречались у Present-событий цели.
    pub present_flags: BTreeMap<u32, u64>,
}

pub struct Collector {
    state: Mutex<State>,
    target_pid: Option<u32>,
    qpc_freq: i64,
    dxgi_present_id: u16,
    d3d9_present_id: u16,
    keep_test_presents: bool,
    total_events: AtomicU64,
    target_events: AtomicU64,
    events_lost: AtomicU32,
    buffers_read: AtomicU32,
    /// Отметки о первом срабатывании каждого callback. Отличают «потребитель подключён, но
    /// событий нет» от «потребитель не подключён вовсе» — снаружи эти случаи выглядят одинаково.
    first_buffer_seen: AtomicBool,
    first_event_seen: AtomicBool,
}

impl Collector {
    pub fn new(
        target_pid: Option<u32>,
        qpc_freq: i64,
        dxgi_id: u16,
        d3d9_id: u16,
        keep_test_presents: bool,
    ) -> Self {
        Collector {
            state: Mutex::new(State::default()),
            target_pid,
            qpc_freq,
            dxgi_present_id: dxgi_id,
            d3d9_present_id: d3d9_id,
            keep_test_presents,
            total_events: AtomicU64::new(0),
            target_events: AtomicU64::new(0),
            events_lost: AtomicU32::new(0),
            buffers_read: AtomicU32::new(0),
            first_buffer_seen: AtomicBool::new(false),
            first_event_seen: AtomicBool::new(false),
        }
    }

    pub fn total_events(&self) -> u64 {
        self.total_events.load(Ordering::Relaxed)
    }

    pub fn target_events(&self) -> u64 {
        self.target_events.load(Ordering::Relaxed)
    }

    pub fn events_lost(&self) -> u32 {
        self.events_lost.load(Ordering::Relaxed)
    }

    pub fn buffers_read(&self) -> u32 {
        self.buffers_read.load(Ordering::Relaxed)
    }

    /// Снимок числа кадров для живой строки состояния — без блокировки надолго.
    pub fn frames_so_far(&self) -> u64 {
        let state = self.lock();
        state.chains.values().map(|c| c.frames.len() as u64).sum()
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Отравленный мьютекс здесь не повод падать: данные спайка остаются осмысленными.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn present_start_id(&self, provider: ProviderKey) -> Option<u16> {
        if provider == ProviderKey::from_guid(&PROVIDER_DXGI.guid) {
            Some(self.dxgi_present_id)
        } else if provider == ProviderKey::from_guid(&PROVIDER_D3D9.guid) {
            Some(self.d3d9_present_id)
        } else {
            None
        }
    }

    fn on_event(&self, record: &EVENT_RECORD) {
        let header = &record.EventHeader;
        if !self.first_event_seen.swap(true, Ordering::Relaxed) {
            eprintln!("[consumer] первое событие доставлено");
        }
        self.total_events.fetch_add(1, Ordering::Relaxed);

        let provider = ProviderKey::from_guid(&header.ProviderId);
        let key = EventKey {
            pid: header.ProcessId,
            provider,
            id: header.EventDescriptor.Id,
            version: header.EventDescriptor.Version,
            opcode: header.EventDescriptor.Opcode,
        };

        let is_target = match self.target_pid {
            None => true,
            Some(pid) => header.ProcessId == pid,
        };
        if is_target {
            self.target_events.fetch_add(1, Ordering::Relaxed);
        }

        let mut state = self.lock();
        *state.events.entry(key).or_insert(0) += 1;
        if state.first_ts.is_none() {
            state.first_ts = Some(header.TimeStamp);
        }
        state.last_ts = Some(header.TimeStamp);

        // Frametime считаем только когда цель задана явно: в режиме `--all` события идут от
        // десятков процессов, и общая дельта не значит ничего.
        let Some(target) = self.target_pid else { return };
        if header.ProcessId != target {
            return;
        }
        if self.present_start_id(provider) != Some(header.EventDescriptor.Id) {
            return;
        }

        let Some(payload) = present_payload(record) else {
            state.presents_without_chain += 1;
            return;
        };
        if let Some(flags) = payload.flags {
            *state.present_flags.entry(flags).or_insert(0) += 1;
        }

        // Тестовый Present кадра не выводит. Если его считать, частота удваивается, а между
        // «кадрами» появляются дельты в сотые доли миллисекунды. PresentMon такие вызовы
        // отбрасывает, и мы тоже — но только у DXGI: у D3D9 бит 0 означает другое.
        let is_dxgi = provider == ProviderKey::from_guid(&PROVIDER_DXGI.guid);
        let is_test = payload.flags.is_some_and(|flags| flags & DXGI_PRESENT_TEST != 0);
        if is_dxgi && is_test && !self.keep_test_presents {
            state.filtered_test_presents += 1;
            return;
        }
        let chain_id = payload.chain;

        let freq = self.qpc_freq;
        let chain = state.chains.entry(chain_id).or_default();
        chain.provider = Some(provider);
        chain.presents += 1;
        if chain.first_ts.is_none() {
            chain.first_ts = Some(header.TimeStamp);
        }
        chain.last_seen_ts = Some(header.TimeStamp);
        if let Some(prev) = chain.last_ts {
            let delta = header.TimeStamp.saturating_sub(prev);
            if delta > 0 {
                let ms = (delta as f64) * 1000.0 / (freq as f64);
                if ms.is_finite() && ms > 0.0 {
                    chain.frames.push(ms as f32);
                }
            }
        }
        chain.last_ts = Some(header.TimeStamp);
    }
}

/// Флаг `DXGI_PRESENT_TEST`. Такой вызов `Present` ничего не выводит на экран — это проверка
/// на перекрытие окна. Событие ETW он порождает наравне с настоящим кадром.
pub const DXGI_PRESENT_TEST: u32 = 0x0000_0001;

/// Начало payload у Present-событий обоих провайдеров: указатель на цепочку обмена (8 байт на
/// x64), затем флаги (4 байта). У DXGI дальше идёт ещё `SyncInterval`.
///
/// PLAN.md §2.9 говорит, что payload разбирать не обязательно, и для PID с таймстемпом это так.
/// Но без флагов нельзя отличить настоящий кадр от тестового вызова — а игры их делают, и
/// счёт удваивается (см. `COVERAGE.md`, Rise of the Tomb Raider).
struct PresentPayload {
    chain: u64,
    flags: Option<u32>,
}

fn present_payload(record: &EVENT_RECORD) -> Option<PresentPayload> {
    if record.UserData.is_null() || (record.UserDataLength as usize) < 8 {
        return None;
    }
    let chain = unsafe { record.UserData.cast::<u64>().read_unaligned() };
    let flags = if (record.UserDataLength as usize) >= 12 {
        Some(unsafe { record.UserData.cast::<u8>().add(8).cast::<u32>().read_unaligned() })
    } else {
        None
    };
    Some(PresentPayload { chain, flags })
}

/// Callback потребителя. Контекст приходит из `EVENT_TRACE_LOGFILEW.Context`.
pub unsafe extern "system" fn event_record_callback(record: *mut EVENT_RECORD) {
    if record.is_null() {
        return;
    }
    let record = unsafe { &*record };
    let collector = record.UserContext as *const Collector;
    if collector.is_null() {
        return;
    }
    unsafe { (*collector).on_event(record) };
}

/// Вызывается на каждый доставленный буфер. Здесь снимаются потери — тот самый счётчик,
/// ненулевое значение которого объясняет пустой захват (PLAN.md §2.1).
pub unsafe extern "system" fn buffer_callback(logfile: *mut EVENT_TRACE_LOGFILEW) -> u32 {
    if logfile.is_null() {
        return 1;
    }
    let logfile = unsafe { &*logfile };
    let collector = logfile.Context as *const Collector;
    if !collector.is_null() {
        let collector = unsafe { &*collector };
        if !collector.first_buffer_seen.swap(true, Ordering::Relaxed) {
            eprintln!(
                "[consumer] первый буфер доставлен: размер {} КБ, заполнено {} байт",
                logfile.BufferSize / 1024,
                logfile.Filled
            );
        }
        collector.events_lost.fetch_max(logfile.EventsLost, Ordering::Relaxed);
        collector.buffers_read.fetch_max(logfile.BuffersRead, Ordering::Relaxed);
    }
    if crate::etw::stop_requested() { 0 } else { 1 }
}

//! Что именно считать кадром в потоке событий DXGI и D3D9.
//!
//! Модуль чистый: он работает с байтами payload и числами, а не с ETW. Собирается и проверяется
//! на любой платформе — именно здесь лежит всё, на чём легко ошибиться.
//!
//! Все факты ниже сняты измерением в фазе P0, см. `spikes/etw-frames/COVERAGE.md`.

use mh_core::Millis;

use crate::frames::swapchain::SwapChainSelector;

/// Провайдер, от которого пришло событие.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Dxgi,
    D3d9,
}

/// `Microsoft-Windows-DXGI`, `Present_Start`.
///
/// Подтверждено дважды: манифестом (`wevtutil gp Microsoft-Windows-DXGI /ge:true` — события 42 и
/// 43 относятся к задаче 9, opcode 1 и 2) и замером, где их количества совпали ровно.
pub const DXGI_PRESENT_START_ID: u16 = 42;
/// `Microsoft-Windows-D3D9`, `Present_Start`. Проверено на GTA: San Andreas.
pub const D3D9_PRESENT_START_ID: u16 = 1;

/// `Microsoft-Windows-DxgKrnl`, событие 184 — вызов вывода кадра ядром (`D3DKMTPresent`).
///
/// Так кадры видны у OpenGL-игр в окне, которые выводят через GDI-копию: провайдеры рантайма
/// у них молчат, PresentMon теряет большую часть кадров. Проверено на Ion Fury пробником
/// `examples/probe_dxgkrnl`: интервалы события совпали со счётчиком игры при 165, ~175 и ~520
/// FPS, ни одного интервала короче 1 мс (PLAN.md §2.16).
pub const KERNEL_PRESENT_ID: u16 = 184;

/// Сколько рантайм должен молчать, прежде чем кадры берутся из событий ядра. У DirectX-игр в
/// окне ядро тоже сообщает о выводе — считать его вместе с рантаймом значило бы удвоить FPS.
pub const RUNTIME_QUIET_MS: i64 = 1_000;

/// `DXGI_PRESENT_TEST`.
///
/// Такой вызов `Present` **не выводит кадр на экран** — это проверка, не перекрыто ли окно.
/// Событие ETW он при этом порождает наравне с настоящим кадром.
///
/// Rise of the Tomb Raider делает ровно один тестовый вызов на каждый настоящий кадр: гистограмма
/// флагов вышла `0x1×8678, 0x200×8678`. Без фильтра счёт удваивался — 595 FPS вместо 300 по
/// оверлею NVIDIA, с дельтами по 0.04 мс между «кадрами».
///
/// **Но бит `0x1` в событии DXGI не всегда означает этот флаг.** Devil May Cry 4 SE — чистый D3D9,
/// выводимый через DXGI: провайдер D3D9 молчит, а в событии DXGI бит `0x1` стоит на **каждом**
/// настоящем кадре (1803 из 1803, ряд ровно 179.9 FPS при 178 по PresentMon). Похоже, это
/// прокинутый `D3DPRESENT_DONOTWAIT`. Отличить два случая можно только по соседству — см.
/// [`FrameAccumulator`]. PresentMon этой неоднозначности не знает: кадры он считает по событиям
/// ядра (DxgKrnl), а не по событию рантайма.
pub const DXGI_PRESENT_TEST: u32 = 0x0000_0001;
/// `DXGI_PRESENT_ALLOW_TEARING`. Встречается постоянно и кадром быть не мешает.
pub const DXGI_PRESENT_ALLOW_TEARING: u32 = 0x0000_0200;

impl ProviderKind {
    /// Идентификатор события «начало Present» у этого провайдера.
    pub fn present_start_id(self) -> u16 {
        match self {
            ProviderKind::Dxgi => DXGI_PRESENT_START_ID,
            ProviderKind::D3d9 => D3D9_PRESENT_START_ID,
        }
    }

    pub fn is_present_start(self, event_id: u16) -> bool {
        event_id == self.present_start_id()
    }
}

/// Начало payload у Present-событий обоих провайдеров.
///
/// Первым идёт указатель на цепочку обмена (8 байт на x64), затем флаги (4 байта). У DXGI
/// дальше лежит `SyncInterval`, который нам не нужен.
///
/// Утверждение PLAN.md §2.9 «payload разбирать не обязательно» верно для PID и таймстемпа —
/// они и правда есть в `EventHeader`. Но **для честного счёта кадров разбор обязателен**: без
/// флагов не отличить настоящий кадр от тестового вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentPayload {
    pub swap_chain: u64,
    /// `None`, если payload короче двенадцати байт.
    pub flags: Option<u32>,
}

/// `D3DPRESENT_DONOTWAIT` — у D3D9 бит `0x1` означает **неблокирующий** Present, а не тест.
///
/// У провайдера D3D9 бит поэтому не проверяется вовсе: игра, которая всегда вызывает Present с
/// этим флагом, иначе теряла бы каждый кадр.
pub const D3DPRESENT_DONOTWAIT: u32 = 0x0000_0001;

impl PresentPayload {
    /// Стоит ли бит, который **может** означать тестовый вызов.
    ///
    /// Только у DXGI; у D3D9 тот же бит — [`D3DPRESENT_DONOTWAIT`]. Но и у DXGI это лишь
    /// кандидат: решение, тест это или кадр, принимает [`FrameAccumulator`] по соседним вызовам.
    pub fn carries_test_bit(&self, provider: ProviderKind) -> bool {
        provider == ProviderKind::Dxgi
            && self.flags.is_some_and(|flags| flags & DXGI_PRESENT_TEST != 0)
    }
}

/// Сколько последних Present цепочки учитывается при решении, что значит бит `0x1`.
const FLAG_WINDOW: u32 = 32;
/// Раньше этого числа вызовов решение не принимается, а помеченные вызовы пропускаются.
///
/// Пропуск, а не догадка: если первым пришёл тестовый вызов, а следом через 0.04 мс настоящий,
/// поспешное «это кадр» дало бы дельту в сотые доли миллисекунды и отравило бы 1 % low.
const FLAG_MIN_SAMPLES: u32 = 8;
/// Больше цепочек у одного процесса не бывает; предел — лишь защита памяти от мусора.
const MAX_TRACKED_CHAINS: usize = 32;

/// Скользящее окно «был ли у вызова бит `0x1`» по одной цепочке.
#[derive(Debug, Default, Clone, Copy)]
struct FlagWindow {
    /// Младший бит — самый свежий вызов; единица — помеченный.
    bits: u64,
    len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagVerdict {
    /// Рядом идут обычные Present — помеченные вызовы проверяют окно и кадров не выводят.
    Test,
    /// Помеченные вызовы — единственные на цепочке, значит это и есть кадры.
    Frame,
    Undecided,
}

impl FlagWindow {
    fn push(&mut self, flagged: bool) {
        self.bits = (self.bits << 1) | u64::from(flagged);
        self.len = (self.len + 1).min(FLAG_WINDOW);
    }

    /// Порог «четверть»: у Rise of the Tomb Raider обычных вызовов ровно столько же, сколько
    /// помеченных (1 : 1), у DMC4 SE — ни одного. Редкий обычный вызов на цепочке, которая
    /// презентит с флагом, решение не переворачивает.
    fn verdict(&self) -> FlagVerdict {
        if self.len < FLAG_MIN_SAMPLES {
            return FlagVerdict::Undecided;
        }
        let mask = (1u64 << self.len) - 1;
        let flagged = (self.bits & mask).count_ones();
        let plain = self.len - flagged;
        if plain * 4 >= flagged { FlagVerdict::Test } else { FlagVerdict::Frame }
    }
}

/// Разбирает начало payload. `None`, если байт меньше, чем на указатель.
pub fn parse_present_payload(user_data: &[u8]) -> Option<PresentPayload> {
    if user_data.len() < 8 {
        return None;
    }
    let swap_chain = u64::from_le_bytes(user_data[..8].try_into().ok()?);
    let flags = if user_data.len() >= 12 {
        Some(u32::from_le_bytes(user_data[8..12].try_into().ok()?))
    } else {
        None
    };
    Some(PresentPayload { swap_chain, flags })
}

/// Счётчики за сеанс.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    /// Present-событий цели всего.
    pub presents: u64,
    /// Помеченных битом [`DXGI_PRESENT_TEST`] и не засчитанных: тестовых либо ещё не
    /// распознанных в первые вызовы цепочки.
    pub test_presents: u64,
    /// Помеченных тем же битом, но признанных кадрами — цепочка презентит только с ним.
    pub flagged_frames: u64,
    /// Отброшенных как принадлежащие другой цепочке обмена.
    pub other_chain: u64,
    /// Без разбираемого payload.
    pub malformed: u64,
    /// Получившихся кадров (дельт).
    pub frames: u64,
    /// Событий вывода от ядра у цели.
    pub kernel_presents: u64,
    /// Кадров, посчитанных по ядру — пока рантайм молчал.
    pub kernel_frames: u64,
}

/// Превращает поток Present-событий в frametime.
///
/// Состояние — выбранная цепочка и таймстемп её последнего Present. Смена цепочки обнуляет
/// таймстемп: дельта между кадрами **разных** цепочек не означает ничего.
///
/// Бит `0x1` у DXGI решается **по соседству на той же цепочке**, а не по самому биту: если рядом
/// идут обычные Present, помеченные — тестовые (Rise of the Tomb Raider); если помеченные —
/// единственные, это кадры (DMC4 SE, D3D9 через DXGI). Это эвристика, выведенная из двух
/// измеренных случаев, а не документированное правило.
#[derive(Debug)]
pub struct FrameAccumulator {
    chains: SwapChainSelector<u64>,
    last_present_qpc: Option<i64>,
    qpc_frequency: i64,
    keep_test_presents: bool,
    counters: Counters,
    flag_windows: std::collections::HashMap<u64, FlagWindow>,
    /// Когда цель в последний раз говорила через рантайм (или когда пришло первое событие ядра).
    runtime_heard_qpc: Option<i64>,
    last_kernel_qpc: Option<i64>,
}

impl FrameAccumulator {
    /// `qpc_frequency` — частота `QueryPerformanceCounter`. Сессия создаётся с
    /// `Wnode.ClientContext = 1`, поэтому таймстемпы событий приходят прямо в единицах QPC.
    pub fn new(qpc_frequency: i64) -> Self {
        Self {
            chains: SwapChainSelector::new(),
            last_present_qpc: None,
            qpc_frequency: if qpc_frequency > 0 { qpc_frequency } else { 10_000_000 },
            keep_test_presents: false,
            counters: Counters::default(),
            flag_windows: std::collections::HashMap::new(),
            runtime_heard_qpc: None,
            last_kernel_qpc: None,
        }
    }

    /// Не отбрасывать тестовые вызовы. Нужно только для диагностики: счёт кадров при этом
    /// завышается.
    pub fn keep_test_presents(mut self, keep: bool) -> Self {
        self.keep_test_presents = keep;
        self
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn selected_chain(&self) -> Option<u64> {
        self.chains.selected().copied()
    }

    pub fn chain_switches(&self) -> u32 {
        self.chains.switches()
    }

    /// Начинает новый сеанс: история прошлой игры не должна жить в новой.
    pub fn reset(&mut self) {
        self.chains.reset();
        self.last_present_qpc = None;
        self.counters = Counters::default();
        self.flag_windows.clear();
        self.runtime_heard_qpc = None;
        self.last_kernel_qpc = None;
    }

    /// Событие вывода от ядра. Кадр — только если рантайм молчит не меньше
    /// [`RUNTIME_QUIET_MS`]; отсчёт тишины начинается и с первого события ядра, чтобы событие
    /// рантайма, доставленное чуть позже, не успело удвоить счёт.
    pub fn on_kernel_present(&mut self, timestamp_qpc: i64) -> Option<f64> {
        self.counters.kernel_presents += 1;
        let quiet_since = *self.runtime_heard_qpc.get_or_insert(timestamp_qpc);
        let quiet_qpc = RUNTIME_QUIET_MS * self.qpc_frequency / 1_000;
        if timestamp_qpc - quiet_since < quiet_qpc {
            self.last_kernel_qpc = None;
            return None;
        }
        let previous = self.last_kernel_qpc.replace(timestamp_qpc)?;
        let delta = timestamp_qpc.checked_sub(previous)?;
        if delta <= 0 {
            return None;
        }
        let frametime_ms = delta as f64 * 1000.0 / self.qpc_frequency as f64;
        self.counters.kernel_frames += 1;
        Some(frametime_ms)
    }

    /// Решает, засчитывать ли помеченный вызов, и заодно учитывает любой вызов DXGI в окне.
    ///
    /// `true` — вызов дальше не обрабатывается.
    fn skip_as_test(&mut self, provider: ProviderKind, payload: &PresentPayload) -> bool {
        if provider != ProviderKind::Dxgi {
            return false;
        }
        let flagged = payload.carries_test_bit(provider);
        if self.flag_windows.len() >= MAX_TRACKED_CHAINS
            && !self.flag_windows.contains_key(&payload.swap_chain)
        {
            self.flag_windows.clear();
        }
        let window = self.flag_windows.entry(payload.swap_chain).or_default();
        window.push(flagged);
        if !flagged || self.keep_test_presents {
            return false;
        }
        match window.verdict() {
            FlagVerdict::Frame => {
                self.counters.flagged_frames += 1;
                false
            }
            FlagVerdict::Test | FlagVerdict::Undecided => {
                self.counters.test_presents += 1;
                true
            }
        }
    }

    /// Обрабатывает одно событие «начало Present».
    ///
    /// `timestamp_qpc` — `EventHeader.TimeStamp`, `now_ms` — монотонные миллисекунды для
    /// политики выбора цепочки. Возвращает frametime в миллисекундах, когда получилась дельта.
    pub fn on_present_start(
        &mut self,
        provider: ProviderKind,
        timestamp_qpc: i64,
        user_data: &[u8],
        now_ms: Millis,
    ) -> Option<f64> {
        self.counters.presents += 1;
        // Рантайм заговорил — кадры снова считаются по нему, а цепочка ядра начинается заново.
        self.runtime_heard_qpc = Some(timestamp_qpc);
        self.last_kernel_qpc = None;

        let Some(payload) = parse_present_payload(user_data) else {
            self.counters.malformed += 1;
            return None;
        };

        if self.skip_as_test(provider, &payload) {
            return None;
        }

        let previous_chain = self.chains.selected().copied();
        if !self.chains.accept(&payload.swap_chain, now_ms) {
            self.counters.other_chain += 1;
            return None;
        }
        if previous_chain != Some(payload.swap_chain) {
            // Первая цепочка либо переезд на другую: предыдущий таймстемп больше не наш.
            self.last_present_qpc = None;
        }

        let previous = self.last_present_qpc.replace(timestamp_qpc)?;
        let delta = timestamp_qpc.checked_sub(previous)?;
        if delta <= 0 {
            return None;
        }
        let frametime_ms = delta as f64 * 1000.0 / self.qpc_frequency as f64;
        if !frametime_ms.is_finite() || frametime_ms <= 0.0 {
            return None;
        }
        self.counters.frames += 1;
        Some(frametime_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QPC: i64 = 10_000_000;

    /// Ion Fury: только ядро, 165 FPS. Первая секунда — ожидание тишины рантайма.
    #[test]
    fn kernel_presents_become_frames_when_the_runtime_is_silent() {
        let mut accumulator = FrameAccumulator::new(QPC);
        let step = QPC / 165;
        let mut frames = Vec::new();
        for index in 0..400 {
            if let Some(ms) = accumulator.on_kernel_present(index * step) {
                frames.push(ms);
            }
        }
        // Первые ~165 событий уходят на ожидание, дальше — кадр на событие.
        assert!((230..=236).contains(&frames.len()), "{}", frames.len());
        assert!(frames.iter().all(|ms| (ms - 1000.0 / 165.0).abs() < 0.01));
        assert_eq!(accumulator.counters().kernel_presents, 400);
    }

    /// DirectX-игра в окне: ядро сообщает о выводе вместе с рантаймом — кадры только по рантайму.
    #[test]
    fn kernel_presents_never_double_a_runtime_stream() {
        let mut accumulator = FrameAccumulator::new(QPC);
        let step = QPC / 100;
        let mut frames = 0;
        for index in 0..500 {
            let qpc = index * step;
            if accumulator.on_present_start(ProviderKind::Dxgi, qpc, &payload(0xAA, 0), 0).is_some()
            {
                frames += 1;
            }
            if accumulator.on_kernel_present(qpc + step / 2).is_some() {
                frames += 1000;
            }
        }
        assert_eq!(frames, 499, "кадры только от рантайма");
        assert_eq!(accumulator.counters().kernel_frames, 0);
    }

    /// Рантайм замолчал — через секунду кадры идут из ядра, без гигантской первой дельты.
    #[test]
    fn a_silent_runtime_hands_over_to_the_kernel_cleanly() {
        let mut accumulator = FrameAccumulator::new(QPC);
        let step = QPC / 100;
        for index in 0..100 {
            accumulator.on_present_start(ProviderKind::Dxgi, index * step, &payload(0xAA, 0), 0);
        }
        let start = 100 * step;
        let mut frames = Vec::new();
        for index in 0..300 {
            if let Some(ms) = accumulator.on_kernel_present(start + index * step) {
                frames.push(ms);
            }
        }
        assert!(!frames.is_empty());
        assert!(frames.iter().all(|ms| (ms - 10.0).abs() < 0.01), "{frames:?}");
    }

    /// Payload так и выглядит в жизни: указатель, затем флаги, затем SyncInterval.
    fn payload(chain: u64, flags: u32) -> Vec<u8> {
        let mut bytes = chain.to_le_bytes().to_vec();
        bytes.extend_from_slice(&flags.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes
    }

    fn accumulator() -> FrameAccumulator {
        FrameAccumulator::new(QPC)
    }

    /// Один кадр при 60 Гц — одна шестидесятая секунды в единицах QPC.
    const FRAME_60HZ: i64 = QPC / 60;

    // --- разбор payload ---

    #[test]
    fn the_payload_yields_the_chain_and_the_flags() {
        let parsed = parse_present_payload(&payload(0x27A34FD9080, DXGI_PRESENT_ALLOW_TEARING))
            .expect("payload разобран");
        assert_eq!(parsed.swap_chain, 0x27A34FD9080);
        assert_eq!(parsed.flags, Some(DXGI_PRESENT_ALLOW_TEARING));
        assert!(!parsed.carries_test_bit(ProviderKind::Dxgi));
    }

    #[test]
    fn a_payload_with_only_a_pointer_has_no_flags() {
        let parsed = parse_present_payload(&0xAAAAu64.to_le_bytes()).expect("указатель есть");
        assert_eq!(parsed.swap_chain, 0xAAAA);
        assert_eq!(parsed.flags, None);
        assert!(!parsed.carries_test_bit(ProviderKind::Dxgi), "без флагов тестовым считать нельзя");
    }

    #[test]
    fn a_short_payload_is_not_a_present() {
        assert_eq!(parse_present_payload(&[]), None);
        assert_eq!(parse_present_payload(&[1, 2, 3, 4]), None);
    }

    // --- идентификаторы событий ---

    #[test]
    fn present_start_ids_match_the_measured_ones() {
        assert!(ProviderKind::Dxgi.is_present_start(42));
        assert!(!ProviderKind::Dxgi.is_present_start(43), "43 — это Present_Stop");
        assert!(ProviderKind::D3d9.is_present_start(1));
        assert!(!ProviderKind::D3d9.is_present_start(2));
    }

    // --- счёт кадров ---

    #[test]
    fn the_first_present_has_nothing_to_subtract_from() {
        let mut accumulator = accumulator();
        assert_eq!(
            accumulator.on_present_start(ProviderKind::Dxgi, 1_000, &payload(0xAAAA, 0), 0),
            None
        );
        assert_eq!(accumulator.counters().frames, 0);
        assert_eq!(accumulator.counters().presents, 1);
    }

    #[test]
    fn consecutive_presents_yield_a_frametime() {
        let mut accumulator = accumulator();
        accumulator.on_present_start(ProviderKind::Dxgi, 0, &payload(0xAAAA, 0), 0);
        let frametime = accumulator
            .on_present_start(ProviderKind::Dxgi, FRAME_60HZ, &payload(0xAAAA, 0), 16)
            .expect("вторая метка даёт дельту");
        assert!((frametime - 16.6667).abs() < 0.001, "frametime = {frametime}");
        assert_eq!(accumulator.counters().frames, 1);
    }

    /// Главная ловушка источника: тестовый Present удваивал счёт кадров.
    #[test]
    fn test_presents_are_not_frames() {
        let mut accumulator = accumulator();
        let mut qpc = 0;
        // Игра делает по одному тестовому вызову на каждый настоящий кадр.
        for _ in 0..100 {
            accumulator.on_present_start(
                ProviderKind::Dxgi,
                qpc,
                &payload(0xAAAA, DXGI_PRESENT_TEST),
                0,
            );
            qpc += FRAME_60HZ / 400; // тестовый вызов идёт почти вплотную к настоящему
            accumulator.on_present_start(
                ProviderKind::Dxgi,
                qpc,
                &payload(0xAAAA, DXGI_PRESENT_ALLOW_TEARING),
                0,
            );
            qpc += FRAME_60HZ;
        }
        let counters = accumulator.counters();
        assert_eq!(counters.presents, 200);
        assert_eq!(counters.test_presents, 100);
        assert_eq!(counters.frames, 99, "кадров столько, сколько настоящих Present минус первый");
    }

    /// Без фильтра та же последовательность даёт вдвое больше «кадров» и дельты в сотые доли
    /// миллисекунды — ровно то, что наблюдалось до исправления.
    #[test]
    fn keeping_test_presents_doubles_the_count_as_it_did_in_the_wild() {
        let mut accumulator = accumulator().keep_test_presents(true);
        let mut qpc = 0;
        let mut shortest = f64::MAX;
        let mut note = |frametime: Option<f64>| {
            if let Some(value) = frametime {
                shortest = shortest.min(value);
            }
        };
        for _ in 0..100 {
            // Короткая дельта возникает на переходе «тестовый вызов → настоящий кадр», то есть
            // возвращает её второй вызов, а не первый. Смотреть надо на оба.
            note(accumulator.on_present_start(
                ProviderKind::Dxgi,
                qpc,
                &payload(0xAAAA, DXGI_PRESENT_TEST),
                0,
            ));
            qpc += FRAME_60HZ / 400;
            note(accumulator.on_present_start(ProviderKind::Dxgi, qpc, &payload(0xAAAA, 0), 0));
            qpc += FRAME_60HZ;
        }
        assert_eq!(accumulator.counters().frames, 199);
        assert!(shortest < 0.1, "появляются дельты в сотые доли миллисекунды: {shortest}");
    }

    /// Регрессия: у D3D9 бит `0x1` — это `D3DPRESENT_DONOTWAIT`. Каждый такой Present — кадр.
    ///
    /// До исправления фильтр тестовых вызовов действовал и на D3D9, и Devil May Cry 4 SE давала
    /// ноль кадров при 178 FPS по PresentMon.
    #[test]
    fn a_d3d9_donotwait_present_is_a_frame_not_a_test() {
        let mut accumulator = accumulator();
        let mut qpc = 0;
        for index in 0..10 {
            accumulator.on_present_start(
                ProviderKind::D3d9,
                qpc,
                &payload(0xAAAA, D3DPRESENT_DONOTWAIT),
                index,
            );
            qpc += FRAME_60HZ;
        }
        let counters = accumulator.counters();
        assert_eq!(counters.test_presents, 0, "у D3D9 тестовых вызовов не бывает");
        assert_eq!(counters.frames, 9);
    }

    #[test]
    fn the_same_bit_means_a_test_only_for_dxgi() {
        let parsed = parse_present_payload(&payload(0xAAAA, 0x1)).unwrap();
        assert!(parsed.carries_test_bit(ProviderKind::Dxgi));
        assert!(!parsed.carries_test_bit(ProviderKind::D3d9));
    }

    // --- что значит бит 0x1 у DXGI: решает соседство ---

    /// DMC4 SE: D3D9, выведенный через DXGI. Бит стоит на каждом настоящем кадре, обычных вызовов
    /// нет вовсе. Замер: 1803 из 1803 помеченных, ровно 179.9 FPS, PresentMon — 178.
    #[test]
    fn a_chain_that_presents_only_flagged_is_presenting_frames() {
        let mut accumulator = accumulator();
        let mut qpc = 0;
        for index in 0..100 {
            accumulator.on_present_start(
                ProviderKind::Dxgi,
                qpc,
                &payload(0xAAAA, DXGI_PRESENT_TEST),
                index,
            );
            qpc += FRAME_60HZ;
        }
        let counters = accumulator.counters();
        // Первые семь вызовов пропущены, пока окно не наберёт образцы, восьмой — первый кадр
        // без дельты, дальше каждый вызов даёт дельту.
        assert_eq!(counters.test_presents, (FLAG_MIN_SAMPLES - 1) as u64);
        assert_eq!(counters.flagged_frames, 100 - (FLAG_MIN_SAMPLES - 1) as u64);
        assert_eq!(counters.frames, 100 - FLAG_MIN_SAMPLES as u64);
    }

    /// Те же кадры дают честный frametime, а не только правильный счёт.
    #[test]
    fn flagged_frames_keep_their_real_frametime() {
        let mut accumulator = accumulator();
        let mut qpc = 0;
        let mut frametimes = Vec::new();
        for index in 0..50 {
            if let Some(value) = accumulator.on_present_start(
                ProviderKind::Dxgi,
                qpc,
                &payload(0xAAAA, DXGI_PRESENT_TEST),
                index,
            ) {
                frametimes.push(value);
            }
            qpc += FRAME_60HZ;
        }
        assert!(!frametimes.is_empty());
        assert!(frametimes.iter().all(|value| (value - 16.6667).abs() < 0.001), "{frametimes:?}");
    }

    /// В начале окна решение не принимается. Иначе при порядке «тест, кадр» первый тест
    /// засчитался бы кадром, и следующий настоящий Present дал бы дельту в 0.04 мс.
    #[test]
    fn a_leading_test_present_never_produces_a_microsecond_frame() {
        let mut accumulator = accumulator();
        let mut qpc = 0;
        let mut shortest = f64::MAX;
        for _ in 0..50 {
            for flags in [DXGI_PRESENT_TEST, 0] {
                if let Some(value) = accumulator.on_present_start(
                    ProviderKind::Dxgi,
                    qpc,
                    &payload(0xAAAA, flags),
                    0,
                ) {
                    shortest = shortest.min(value);
                }
                qpc += if flags == DXGI_PRESENT_TEST { FRAME_60HZ / 400 } else { FRAME_60HZ };
            }
        }
        assert!(shortest > 16.0, "появилась дельта в доли миллисекунды: {shortest}");
        assert_eq!(accumulator.counters().flagged_frames, 0, "при соседстве 1:1 это тесты");
    }

    /// Редкий обычный вызов на цепочке, которая презентит с флагом, решения не переворачивает.
    #[test]
    fn an_occasional_plain_present_does_not_hide_a_flagged_chain() {
        let mut accumulator = accumulator();
        let mut qpc = 0;
        for index in 0..200u64 {
            let flags = if index % 50 == 25 { 0 } else { DXGI_PRESENT_TEST };
            accumulator.on_present_start(ProviderKind::Dxgi, qpc, &payload(0xAAAA, flags), index);
            qpc += FRAME_60HZ;
        }
        let counters = accumulator.counters();
        assert!(counters.frames > 180, "кадров {} из 200", counters.frames);
        assert_eq!(counters.test_presents, (FLAG_MIN_SAMPLES - 1) as u64);
    }

    /// Решение принимается по каждой цепочке отдельно: тестовые вызовы одной не влияют на
    /// толкование другой.
    #[test]
    fn chains_are_judged_independently() {
        let mut accumulator = accumulator();
        for _ in 0..40 {
            accumulator.on_present_start(ProviderKind::Dxgi, 0, &payload(0xAAAA, 0), 0);
            accumulator.on_present_start(
                ProviderKind::Dxgi,
                0,
                &payload(0xBBBB, DXGI_PRESENT_TEST),
                0,
            );
        }
        let windows = &accumulator.flag_windows;
        assert_eq!(windows[&0xAAAA].verdict(), FlagVerdict::Test, "обычных полно");
        assert_eq!(windows[&0xBBBB].verdict(), FlagVerdict::Frame, "только помеченные");
    }

    #[test]
    fn the_window_threshold_is_a_quarter() {
        let window_with = |plain: u32| {
            let mut window = FlagWindow::default();
            for index in 0..FLAG_WINDOW {
                window.push(index >= plain);
            }
            window.verdict()
        };
        // 32 вызова: при 7 обычных и 25 помеченных 28 >= 25 — это ещё тесты.
        assert_eq!(window_with(7), FlagVerdict::Test);
        // При 6 обычных и 26 помеченных 24 < 26 — уже кадры.
        assert_eq!(window_with(6), FlagVerdict::Frame);
        assert_eq!(window_with(0), FlagVerdict::Frame);
        assert_eq!(window_with(16), FlagVerdict::Test);
    }

    /// Мусорные адреса цепочек не должны раздувать память.
    #[test]
    fn tracked_chains_are_bounded() {
        let mut accumulator = accumulator();
        for chain in 0..1_000u64 {
            accumulator.on_present_start(ProviderKind::Dxgi, 0, &payload(chain, 0), 0);
        }
        assert!(accumulator.flag_windows.len() <= MAX_TRACKED_CHAINS);
    }

    // --- цепочки обмена ---

    #[test]
    fn frames_of_another_chain_are_not_mixed_in() {
        let mut accumulator = accumulator();
        accumulator.on_present_start(ProviderKind::Dxgi, 0, &payload(0xAAAA, 0), 0);
        assert_eq!(
            accumulator.on_present_start(
                ProviderKind::Dxgi,
                FRAME_60HZ / 2,
                &payload(0xBBBB, 0),
                8
            ),
            None
        );
        assert_eq!(accumulator.counters().other_chain, 1);

        let frametime = accumulator
            .on_present_start(ProviderKind::Dxgi, FRAME_60HZ, &payload(0xAAAA, 0), 16)
            .expect("своя цепочка продолжается");
        assert!((frametime - 16.6667).abs() < 0.001, "чужой кадр не сбил отсчёт");
    }

    /// После переезда на другую цепочку первая дельта не считается: расстояние между
    /// Present'ами разных цепочек не значит ничего.
    #[test]
    fn a_chain_switch_discards_the_stale_timestamp() {
        let mut accumulator = accumulator();
        accumulator.on_present_start(ProviderKind::Dxgi, 0, &payload(0xAAAA, 0), 0);

        // Старая цепочка замолчала, претендент набирает нужные кадры.
        let mut qpc = 10 * QPC;
        let mut first_after_switch = None;
        for index in 0..30 {
            let produced = accumulator.on_present_start(
                ProviderKind::Dxgi,
                qpc,
                &payload(0xBBBB, 0),
                5_000 + index,
            );
            if accumulator.selected_chain() == Some(0xBBBB) && first_after_switch.is_none() {
                first_after_switch = Some(produced);
            }
            qpc += FRAME_60HZ;
        }
        assert_eq!(accumulator.selected_chain(), Some(0xBBBB));
        assert_eq!(first_after_switch, Some(None), "первый кадр новой цепочки — не дельта");
        assert_eq!(accumulator.chain_switches(), 1);
    }

    // --- устойчивость ---

    #[test]
    fn a_timestamp_that_does_not_move_forward_is_not_a_frame() {
        let mut accumulator = accumulator();
        accumulator.on_present_start(ProviderKind::Dxgi, 1_000, &payload(0xAAAA, 0), 0);
        assert_eq!(
            accumulator.on_present_start(ProviderKind::Dxgi, 1_000, &payload(0xAAAA, 0), 0),
            None
        );
        assert_eq!(
            accumulator.on_present_start(ProviderKind::Dxgi, 500, &payload(0xAAAA, 0), 0),
            None
        );
        assert_eq!(accumulator.counters().frames, 0);
    }

    #[test]
    fn a_malformed_payload_is_counted_and_skipped() {
        let mut accumulator = accumulator();
        assert_eq!(accumulator.on_present_start(ProviderKind::Dxgi, 1_000, &[1, 2], 0), None);
        assert_eq!(accumulator.counters().malformed, 1);
        assert_eq!(accumulator.counters().frames, 0);
    }

    /// Нулевая частота QPC означала бы деление на ноль на каждом кадре.
    #[test]
    fn a_broken_qpc_frequency_does_not_poison_every_frame() {
        let mut accumulator = FrameAccumulator::new(0);
        accumulator.on_present_start(ProviderKind::Dxgi, 0, &payload(0xAAAA, 0), 0);
        let frametime =
            accumulator.on_present_start(ProviderKind::Dxgi, FRAME_60HZ, &payload(0xAAAA, 0), 16);
        assert!(frametime.is_some_and(|value| value.is_finite() && value > 0.0));
    }

    #[test]
    fn a_reset_starts_the_next_game_from_scratch() {
        let mut accumulator = accumulator();
        accumulator.on_present_start(ProviderKind::Dxgi, 0, &payload(0xAAAA, 0), 0);
        accumulator.on_present_start(ProviderKind::Dxgi, FRAME_60HZ, &payload(0xAAAA, 0), 16);
        accumulator.reset();

        assert_eq!(accumulator.counters(), Counters::default());
        assert_eq!(accumulator.selected_chain(), None);
        assert_eq!(
            accumulator.on_present_start(
                ProviderKind::Dxgi,
                FRAME_60HZ * 2,
                &payload(0xBBBB, 0),
                32
            ),
            None,
            "после сброса первый Present снова не даёт дельты"
        );
    }
}

//! Потоки опроса и публикация снимка.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use mh_core::{
    Aggregator, FpsReason, GraphBuilder, Millis, SampleTier, Snapshot, TargetResolution,
    session_name,
};
use mh_platform::etw::{install_panic_cleanup, stop_by_name, sweep_orphans};
use mh_platform::job::KillOnCloseJob;
use mh_sources::clock::{Clock, MonotonicClock};
use mh_sources::frames::capture::FrameCapture;
use mh_sources::frames::etw_dxgi::source::EtwFactory;
use mh_sources::frames::presentmon::child::ProcessLauncher;
use mh_sources::frames::presentmon::command::ExecutableChoice;
use mh_sources::frames::presentmon::source::PresentMonFactory;
use mh_sources::frames::source::FrameSourceFactory;
use mh_sources::hardware::sampler::HardwareSampler;

/// Частоты опроса — те же, что отлажены в старом проекте (`PollingConfig.kt`).
const LOAD_INTERVAL_MS: Millis = 500;
const SLOW_INTERVAL_MS: Millis = 1_000;
const FRAMES_INTERVAL_MS: Millis = 250;

pub struct EngineConfig {
    /// Разложенный на диск вложенный PresentMon.
    pub presentmon: PathBuf,
    /// Свой PresentMon пользователя, если задан.
    pub presentmon_override: Option<PathBuf>,
}

type Notify = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    aggregator: Mutex<Aggregator>,
    /// Кого мерить. Решает не движок: в службе нет окна в фокусе (сессия 0), поэтому цель
    /// выбирает UI — через [`crate::TargetWatcher`] — и присылает готовой.
    target: Mutex<TargetResolution>,
    stop: AtomicBool,
    clock: Arc<dyn Clock>,
    notify: Notify,
}

impl Shared {
    fn aggregator(&self) -> MutexGuard<'_, Aggregator> {
        self.aggregator.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Спит до `deadline`, просыпаясь раньше при остановке.
    fn sleep_until(&self, deadline: Millis) {
        while !self.stop.load(Ordering::Relaxed) {
            let now = self.clock.now_ms();
            if now >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis((deadline - now).min(50)));
        }
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    session: String,
    _job: Option<Arc<KillOnCloseJob>>,
}

impl Engine {
    /// Запускает опрос. `notify` вызывается из потоков движка после каждого обновления снимка.
    pub fn start(config: EngineConfig, notify: impl Fn() + Send + Sync + 'static) -> Engine {
        let session = session_name(std::process::id());

        // Защита от сирот (PLAN.md §2.12), от внешнего слоя к внутреннему: уборка после прошлого
        // запуска, остановка сессий при панике, job object для PresentMon, Drop источников.
        // Обработчика консоли нет: у оконного приложения консоли нет.
        let _ = sweep_orphans(&session);
        install_panic_cleanup();
        let job = KillOnCloseJob::new().ok().map(Arc::new);

        let shared = Arc::new(Shared {
            aggregator: Mutex::new(Aggregator::new()),
            target: Mutex::new(no_target()),
            stop: AtomicBool::new(false),
            clock: MonotonicClock::shared(),
            notify: Arc::new(notify),
        });

        let hardware = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("mh-hardware".into())
                .spawn(move || hardware_loop(&shared))
                .expect("поток опроса железа")
        };
        let frames = {
            let shared = Arc::clone(&shared);
            let capture = build_capture(&config, &session, job.clone(), Arc::clone(&shared.clock));
            std::thread::Builder::new()
                .name("mh-frames".into())
                .spawn(move || frames_loop(&shared, capture))
                .expect("поток опроса кадров")
        };

        Engine { shared, threads: vec![hardware, frames], session, _job: job }
    }

    pub fn snapshot(&self) -> Snapshot {
        let now = self.shared.clock.now_ms();
        self.shared.aggregator().snapshot(now)
    }

    pub fn set_target(&self, target: TargetResolution) {
        *self.shared.target.lock().unwrap_or_else(|e| e.into_inner()) = target;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        // Источники остановлены вместе с потоком кадров. Своя сессия по имени — на случай, если
        // поток умер, не дойдя до Drop.
        stop_by_name(&self.session);
    }
}

fn hardware_loop(shared: &Shared) {
    let mut sampler = HardwareSampler::open();
    let mut next_slow = 0;
    loop {
        let started = shared.clock.now_ms();
        let load = sampler.sample(SampleTier::Load, started);
        shared.aggregator().submit_hardware(SampleTier::Load, load, started);
        if started >= next_slow {
            let slow = sampler.sample(SampleTier::Slow, started);
            shared.aggregator().submit_hardware(SampleTier::Slow, slow, started);
            next_slow = started + SLOW_INTERVAL_MS;
        }
        (shared.notify)();
        shared.sleep_until(started + LOAD_INTERVAL_MS);
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
    }
}

fn frames_loop(shared: &Shared, mut capture: FrameCapture) {
    let mut graph = GraphBuilder::new();
    loop {
        let now = shared.clock.now_ms();
        let resolution = shared.target.lock().unwrap_or_else(|e| e.into_inner()).clone();
        capture.set_target(resolution.target());
        let mut state = capture.poll(now);
        // Цель не выбрана: объясняем словами трекера, а не общим «жду игру».
        if state.target.is_none()
            && let TargetResolution::Unresolved { reason, detail } = &resolution
        {
            state.reason = Some(*reason);
            state.detail = detail.clone();
        }
        let picture = graph.build(state.session_id, capture.frametimes());
        {
            let mut aggregator = shared.aggregator();
            aggregator.submit_fps(state);
            aggregator.submit_graph(picture);
        }
        (shared.notify)();
        shared.sleep_until(now + FRAMES_INTERVAL_MS);
        if shared.stop.load(Ordering::Relaxed) {
            // Источник останавливается здесь, в своём потоке, до выхода из движка.
            drop(capture);
            return;
        }
    }
}

fn no_target() -> TargetResolution {
    TargetResolution::Unresolved { reason: FpsReason::NoTarget, detail: None }
}

fn build_capture(
    config: &EngineConfig,
    session: &str,
    job: Option<Arc<KillOnCloseJob>>,
    clock: Arc<dyn Clock>,
) -> FrameCapture {
    let launcher = ProcessLauncher {
        on_spawn: job.map(|job| {
            Arc::new(move |pid: u32| {
                // Не вышло — PresentMon переживёт нас только при аварийном завершении, и его
                // сессию уберёт уборка при следующем старте.
                let _ = job.assign(pid);
            }) as Arc<dyn Fn(u32) + Send + Sync>
        }),
    };
    let executable =
        ExecutableChoice::resolve(config.presentmon.clone(), config.presentmon_override.clone());
    let presentmon: Arc<dyn FrameSourceFactory> = Arc::new(PresentMonFactory {
        launcher: Arc::new(launcher),
        executable,
        session_name: session.to_string(),
        clock: Arc::clone(&clock),
        // TerminateProcess не даёт PresentMon закрыть свою сессию — гасим сами (§2.2).
        session_cleanup: Some(Arc::new(|name: &str| {
            stop_by_name(name);
        })),
    });
    let etw: Arc<dyn FrameSourceFactory> =
        Arc::new(EtwFactory { session_name: session.to_string(), clock });
    FrameCapture::new(presentmon, Some(etw))
}

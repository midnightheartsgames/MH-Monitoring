//! Живая проверка источников кадров (критерий приёмки P2).
//!
//! Соединяет всё, что сделано в P1 и P2: `TargetTracker` выбирает цель, `FrameCapture` решает,
//! каким источником её мерить, и раз в секунду печатается то самое `FpsState`, которое потом
//! увидит UI.
//!
//! Запускать **от администратора** — realtime ETW без прав не поднимется (PLAN.md §2.3):
//!
//! ```powershell
//! cargo run -p mh-sources --example live_frames --release -- --source auto
//! cargo run -p mh-sources --example live_frames --release -- --source presentmon --name present-probe.exe
//! cargo run -p mh-sources --example live_frames --release -- --source etw --pid 12345
//! ```
//!
//! Сверять с эталонным `spikes/present-probe` (он сам печатает свой FPS) и с оверлеем NVIDIA.
//! Средний FPS считается по окну в 30 секунд, поэтому после смены сцены он догоняет не сразу.

#[cfg(not(windows))]
fn main() {
    eprintln!("живая проверка требует Windows: ETW и PresentMon есть только там");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows_main::run()
}

#[cfg(windows)]
mod windows_main {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::time::Duration;

    use mh_core::{FpsAvailability, FpsState, TargetSettings, TargetTracker, session_name};
    use mh_platform::console::{install_ctrl_handler, stop_requested};
    use mh_platform::etw::{stop_by_name, sweep_orphans};
    use mh_platform::job::KillOnCloseJob;
    use mh_platform::process::{SystemProcessLookup, foreground_process};
    use mh_sources::clock::{Clock, MonotonicClock};
    use mh_sources::frames::capture::FrameCapture;
    use mh_sources::frames::etw_dxgi::source::EtwFactory;
    use mh_sources::frames::presentmon::child::ProcessLauncher;
    use mh_sources::frames::presentmon::command::ExecutableChoice;
    use mh_sources::frames::presentmon::source::PresentMonFactory;
    use mh_sources::frames::source::FrameSourceFactory;

    const BUNDLED_PRESENTMON: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/presentmon/PresentMon-2.5.1-x64.exe");
    const POLL_MS: u64 = 250;

    #[derive(Clone, Copy, PartialEq)]
    enum SourceChoice {
        Auto,
        PresentMon,
        Etw,
    }

    struct Args {
        source: SourceChoice,
        settings: TargetSettings,
        seconds: u64,
        presentmon: Option<PathBuf>,
    }

    const HELP: &str = "\
Живая проверка источников кадров.

    --source auto|presentmon|etw   auto — PresentMon с откатом на свой ETW (по умолчанию)
    --pid <PID>                    мерить этот процесс
    --name <ИМЯ.exe>               мерить процесс с этим именем
                                   (без --pid и --name цель берётся из окна в фокусе)
    --seconds <N>                  сколько работать, 0 — до Ctrl+C (по умолчанию 30)
    --presentmon <ПУТЬ>            свой PresentMon.exe вместо вложенного

Запускать от администратора.
";

    fn parse() -> Result<Args, String> {
        let mut args = Args {
            source: SourceChoice::Auto,
            settings: TargetSettings::auto(),
            seconds: 30,
            presentmon: None,
        };
        let mut argv = std::env::args().skip(1);
        while let Some(arg) = argv.next() {
            let mut value = || argv.next().ok_or_else(|| format!("{arg} требует значение"));
            match arg.as_str() {
                "--source" => {
                    args.source = match value()?.as_str() {
                        "auto" => SourceChoice::Auto,
                        "presentmon" => SourceChoice::PresentMon,
                        "etw" => SourceChoice::Etw,
                        other => return Err(format!("неизвестный источник: {other}")),
                    }
                }
                "--pid" => {
                    let pid = value()?;
                    args.settings = TargetSettings::manual_by_pid(
                        pid.parse().map_err(|_| format!("не PID: {pid}"))?,
                    );
                }
                "--name" => args.settings = TargetSettings::manual_by_name(value()?),
                "--seconds" => {
                    let seconds = value()?;
                    args.seconds = seconds.parse().map_err(|_| format!("не число: {seconds}"))?;
                }
                "--presentmon" => args.presentmon = Some(PathBuf::from(value()?)),
                "-h" | "--help" => {
                    print!("{HELP}");
                    std::process::exit(0);
                }
                other => return Err(format!("неизвестный аргумент: {other}")),
            }
        }
        Ok(args)
    }

    pub fn run() -> ExitCode {
        let args = match parse() {
            Ok(args) => args,
            Err(message) => {
                eprintln!("{message}\n\n{HELP}");
                return ExitCode::from(2);
            }
        };

        let own_pid = std::process::id();
        let session = session_name(own_pid);

        // Порядок защиты от сирот, от внешнего слоя к внутреннему:
        //  1. уборка при старте — на случай, если прошлый запуск умер не своей смертью;
        //  2. обработчик консоли — гасит сессию по имени при Ctrl+C и закрытии окна;
        //  3. job object — ядро убьёт PresentMon, даже если нас снимут из диспетчера задач;
        //  4. Drop источников — обычный путь выхода.
        println!("[гигиена] при старте: {}", sweep_orphans(&session).describe());
        if !install_ctrl_handler(&session) {
            eprintln!("[гигиена] обработчик Ctrl+C не установлен — прерывать программу не стоит");
        }
        let job = match KillOnCloseJob::new() {
            Ok(job) => Some(Arc::new(job)),
            Err(error) => {
                eprintln!("[гигиена] job object не создан: {error}");
                None
            }
        };

        let clock: Arc<dyn Clock> = MonotonicClock::shared();
        let capture = build_capture(&args, &session, job.clone(), Arc::clone(&clock));
        let Some(mut capture) = capture else {
            return ExitCode::from(1);
        };

        println!(
            "сессия {session}, источник {}, цель: {}",
            match args.source {
                SourceChoice::Auto => "PresentMon → свой ETW",
                SourceChoice::PresentMon => "только PresentMon",
                SourceChoice::Etw => "только свой ETW",
            },
            describe_settings(&args.settings)
        );
        println!("средний FPS — по окну 30 с; остановка — Ctrl+C\n");

        let mut tracker = TargetTracker::new();
        let lookup = SystemProcessLookup;
        let started = clock.now_ms();
        let mut next_print = started;
        let mut last_state = FpsState::INITIAL;

        loop {
            if stop_requested() {
                println!("\nCtrl+C — останавливаюсь");
                break;
            }
            let now = clock.now_ms();
            if args.seconds > 0 && now.saturating_sub(started) >= args.seconds * 1_000 {
                break;
            }

            let foreground = foreground_process(own_pid);
            let resolution = tracker.resolve(now, foreground.as_ref(), &args.settings, &lookup);
            capture.set_target(resolution.target());
            let mut state = capture.poll(now);
            // Цель не выбрана: объясняем почему словами трекера, а не общим «жду игру».
            if state.target.is_none()
                && let mh_core::TargetResolution::Unresolved { reason, detail } = &resolution
            {
                state.reason = Some(*reason);
                state.detail = detail.clone();
            }

            if now >= next_print {
                print_line(now.saturating_sub(started), &state, capture.active_kind());
                // Пока кадров нет, показываем счётчики источника: «ничего не приходит» и
                // «приходит, но всё отбрасывается» — разные поломки.
                if state.target.is_some()
                    && state.availability != FpsAvailability::Available
                    && let Some(diagnostics) = capture.diagnostics()
                {
                    println!("         └ {diagnostics}");
                }
                next_print = now + 1_000;
            }
            last_state = state;
            std::thread::sleep(Duration::from_millis(POLL_MS));
        }

        // Итоговые счётчики источника — и когда кадры шли. По ним видно, сколько вызовов ушло в
        // тестовые, а сколько стало кадрами: при соседстве 1 : 1 это Rise of the Tomb Raider,
        // при нуле тестовых и помеченных кадрах — D3D9, выведенный через DXGI.
        if let Some(diagnostics) = capture.diagnostics() {
            println!("\n[итог источника] {diagnostics}");
        }
        drop(capture);
        // Итоговая проверка гигиены: своя сессия уже остановлена, и уборка обязана не найти
        // ничего нашего. Ненулевое число здесь — дефект, а не норма.
        let after = sweep_orphans("");
        println!("[гигиена] после остановки: {}", after.describe());
        println!(
            "последнее состояние: {:?}, сеанс {}",
            last_state.availability, last_state.session_id
        );
        drop(job);

        if after.stopped.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(3) }
    }

    fn build_capture(
        args: &Args,
        session: &str,
        job: Option<Arc<KillOnCloseJob>>,
        clock: Arc<dyn Clock>,
    ) -> Option<FrameCapture> {
        let launcher = ProcessLauncher {
            on_spawn: job.map(|job| {
                Arc::new(move |pid: u32| {
                    if let Err(error) = job.assign(pid) {
                        eprintln!("[гигиена] PresentMon не помещён в job object: {error}");
                    }
                }) as Arc<dyn Fn(u32) + Send + Sync>
            }),
        };
        let executable =
            ExecutableChoice::resolve(PathBuf::from(BUNDLED_PRESENTMON), args.presentmon.clone());

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

        Some(match args.source {
            SourceChoice::Auto => FrameCapture::new(presentmon, Some(etw)),
            SourceChoice::PresentMon => FrameCapture::new(presentmon, None),
            SourceChoice::Etw => FrameCapture::new(etw, None),
        })
    }

    fn describe_settings(settings: &TargetSettings) -> String {
        match (settings.manual_pid, settings.manual_process.as_deref()) {
            (Some(pid), _) => format!("pid {pid}"),
            (None, Some(name)) => name.to_string(),
            (None, None) => "окно в фокусе".to_string(),
        }
    }

    fn print_line(
        elapsed_ms: u64,
        state: &FpsState,
        source: Option<mh_sources::frames::source::SourceKind>,
    ) {
        let seconds = elapsed_ms / 1_000;
        let source = source.map(|kind| kind.label()).unwrap_or("—");
        let target = state
            .target
            .as_ref()
            .map(|target| format!("{} ({})", target.executable, target.pid))
            .unwrap_or_else(|| "—".to_string());

        let numbers = if state.availability == FpsAvailability::Available {
            let stats = &state.statistics;
            format!(
                "ср. {:>6.1}  тек. {:>6.1}  1% {:>6}  кадров {:>6}",
                stats.average_fps.unwrap_or(0.0),
                stats.current_fps.unwrap_or(0.0),
                stats.low_1_percent_fps.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".into()),
                stats.sample_count,
            )
        } else {
            format!("{:?}", state.availability)
        };

        let message = state.message().map(|m| format!(" · {m}")).unwrap_or_default();
        println!("[{seconds:>4} с] {source:<16} {target:<28} {numbers}{message}");
    }
}

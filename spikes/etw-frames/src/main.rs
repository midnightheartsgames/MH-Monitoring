//! P0 — спайк собственного ETW-потребителя кадров.
//!
//! Отвечает на один вопрос: даёт ли прямое чтение Present-событий DXGI/D3D9 пригодные
//! frametime и на каких графических API это работает. От ответа зависит, имеет ли смысл
//! собственный источник кадров как запасной путь к PresentMon (PLAN.md §2.4, §6/P0).
//!
//! Запускать от администратора: realtime ETW требует прав (PLAN.md §2.3).
//!
//! Типичный сеанс замера:
//!
//! ```text
//! etw-frames.exe --foreground 5 --seconds 20 --label D3D12
//! etw-frames.exe --all --seconds 15          # видно ли вообще Present от Vulkan-игры
//! etw-frames.exe --list                      # список процессов
//! ```
//!
//! Проверить себя можно без единой игры: `spikes/present-probe` — настоящее D3D11-приложение,
//! которое само считает свои `Present`. Его число и число спайка обязаны совпасть.
//!
//! Спайк подметает осиротевшие сессии `MHMonitor-*` при старте и гасит свою на всех путях
//! выхода, включая Ctrl+C и панику: брошенная realtime-сессия ломает захват кадров на всей
//! машине, и это не теория — см. `COVERAGE.md` §1. Проверить руками: `logman query -ets`.
//!
//! Результаты замеров и матрица покрытия: `COVERAGE.md` рядом с этим файлом.

mod collect;
mod etw;
mod procs;
mod stats;
mod sys;

use std::collections::BTreeMap;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

use collect::{
    Collector, D3D9_PRESENT_START_ID, DXGI_PRESENT_START_ID, ProviderKey, buffer_callback,
    event_record_callback,
};
use etw::{
    Consumer, PROVIDER_D3D9, PROVIDER_DXGI, SESSION_PREFIX, SWEEP_PREFIX, Session, StartError,
    install_emergency_cleanup, request_stop, stop_requested, sweep_orphans,
};
use sys::{Win32Error, qpc_frequency};

const EXIT_USAGE: u8 = 2;
const EXIT_NO_FRAMES: u8 = 3;
const EXIT_ACCESS_DENIED: u8 = 4;
const EXIT_ERROR: u8 = 1;

// --- аргументы -----------------------------------------------------------------------------

enum Target {
    Pid(u32),
    Name(String),
    Foreground(u64),
    All,
}

struct Args {
    target: Target,
    seconds: u64,
    label: Option<String>,
    csv: Option<String>,
    top: usize,
    dxgi_id: u16,
    d3d9_id: u16,
    buffers: etw::BufferConfig,
    level: Option<u8>,
    providers: ProviderChoice,
    keep_test_presents: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ProviderChoice {
    Both,
    DxgiOnly,
    D3d9Only,
}

enum Command {
    Measure(Args),
    List(Option<String>),
    SweepOnly,
    Help,
}

const HELP: &str = "\
P0-спайк: собственный ETW-потребитель кадров (DXGI + D3D9).

ИСПОЛЬЗОВАНИЕ:
    etw-frames [ЦЕЛЬ] [ПАРАМЕТРЫ]

ЦЕЛЬ (одна из; по умолчанию --foreground 5):
    --pid <PID>            измерять процесс с этим PID
    --name <ПОДСТРОКА>     найти процесс по части имени exe
    --foreground [СЕК]     подождать СЕК (по умолчанию 5) и взять процесс окна в фокусе
    --all                  не фильтровать: показать, от каких процессов вообще идут Present

ПАРАМЕТРЫ:
    --seconds <N>          длительность замера, 0 — до Ctrl+C (по умолчанию 20)
    --label <ТЕКСТ>        графический API для строки матрицы покрытия, например D3D12
    --csv <ПУТЬ>           выгрузить frametime цели в CSV
    --top <N>              сколько строк показать в режиме --all (по умолчанию 20)
    --dxgi-id <N>          ID события DXGI Present_Start (по умолчанию 42)
    --d3d9-id <N>          ID события D3D9 Present_Start (по умолчанию 1)

ПАРАМЕТРЫ СЕССИИ (0 — оставить на усмотрение ETW, так и по умолчанию):
    --buffer-kb <N>        размер буфера сессии
    --min-buffers <N>      минимальное число буферов
    --max-buffers <N>      максимальное число буферов
    --flush <СЕК>          период принудительной выгрузки буферов
    --level <0..255>       уровень провайдеров (по умолчанию 255)
    --only <dxgi|d3d9>     включить только один провайдер
    --keep-test-presents   не отбрасывать вызовы Present с флагом DXGI_PRESENT_TEST
                           (они не выводят кадр на экран и по умолчанию не считаются)

ПРОЧЕЕ:
    --list [ПОДСТРОКА]     вывести процессы и выйти
    --sweep-only           убрать осиротевшие сессии префикса и выйти
    -h, --help             эта справка

КОДЫ ВОЗВРАТА: 0 — кадры получены, 1 — ошибка, 2 — аргументы, 3 — кадров нет,
4 — нет прав на realtime ETW.
";

fn parse_args() -> Result<Command, String> {
    let mut argv = std::env::args().skip(1);
    let mut target: Option<Target> = None;
    let mut seconds = 20u64;
    let mut label = None;
    let mut csv = None;
    let mut top = 20usize;
    let mut dxgi_id = DXGI_PRESENT_START_ID;
    let mut d3d9_id = D3D9_PRESENT_START_ID;
    let mut buffers = etw::BufferConfig::default();
    let mut level: Option<u8> = None;
    let mut providers = ProviderChoice::Both;
    let mut keep_test_presents = false;

    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--sweep-only" => return Ok(Command::SweepOnly),
            "--list" => return Ok(Command::List(argv.next())),
            "--pid" => {
                let value = argv.next().ok_or("--pid требует значение")?;
                let pid = value.parse::<u32>().map_err(|_| format!("не PID: {value}"))?;
                target = Some(Target::Pid(pid));
            }
            "--name" => {
                target = Some(Target::Name(argv.next().ok_or("--name требует значение")?));
            }
            "--foreground" => {
                // Число после --foreground необязательно, поэтому берём его только если
                // следующий аргумент действительно число, а не другой флаг.
                let delay = match argv.next() {
                    Some(next) => match next.parse::<u64>() {
                        Ok(value) => value,
                        Err(_) => return Err(format!("после --foreground ожидались секунды, а не «{next}»")),
                    },
                    None => 5,
                };
                target = Some(Target::Foreground(delay));
            }
            "--all" => target = Some(Target::All),
            "--seconds" => {
                let value = argv.next().ok_or("--seconds требует значение")?;
                seconds = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--label" => label = Some(argv.next().ok_or("--label требует значение")?),
            "--csv" => csv = Some(argv.next().ok_or("--csv требует значение")?),
            "--top" => {
                let value = argv.next().ok_or("--top требует значение")?;
                top = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--dxgi-id" => {
                let value = argv.next().ok_or("--dxgi-id требует значение")?;
                dxgi_id = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--d3d9-id" => {
                let value = argv.next().ok_or("--d3d9-id требует значение")?;
                d3d9_id = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--buffer-kb" => {
                let value = argv.next().ok_or("--buffer-kb требует значение")?;
                buffers.buffer_kb = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--min-buffers" => {
                let value = argv.next().ok_or("--min-buffers требует значение")?;
                buffers.min_buffers = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--max-buffers" => {
                let value = argv.next().ok_or("--max-buffers требует значение")?;
                buffers.max_buffers = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--flush" => {
                let value = argv.next().ok_or("--flush требует значение")?;
                buffers.flush_seconds = value.parse().map_err(|_| format!("не число: {value}"))?;
            }
            "--keep-test-presents" => keep_test_presents = true,
            "--level" => {
                let value = argv.next().ok_or("--level требует значение")?;
                level = Some(value.parse().map_err(|_| format!("не число: {value}"))?);
            }
            "--only" => {
                let value = argv.next().ok_or("--only требует значение")?;
                providers = match value.to_lowercase().as_str() {
                    "dxgi" => ProviderChoice::DxgiOnly,
                    "d3d9" => ProviderChoice::D3d9Only,
                    other => return Err(format!("--only принимает dxgi или d3d9, а не {other}")),
                };
            }
            other => return Err(format!("неизвестный аргумент: {other}")),
        }
    }

    Ok(Command::Measure(Args {
        target: target.unwrap_or(Target::Foreground(5)),
        seconds,
        label,
        csv,
        top,
        dxgi_id,
        d3d9_id,
        buffers,
        level,
        providers,
        keep_test_presents,
    }))
}

// --- точка входа ---------------------------------------------------------------------------

fn main() -> ExitCode {
    match parse_args() {
        Ok(Command::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Command::List(filter)) => {
            list_processes(filter.as_deref());
            ExitCode::SUCCESS
        }
        Ok(Command::SweepOnly) => {
            report_sweep(&sweep_orphans(""));
            ExitCode::SUCCESS
        }
        Ok(Command::Measure(args)) => run(args),
        Err(message) => {
            eprintln!("ошибка аргументов: {message}\n");
            eprint!("{HELP}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

fn list_processes(filter: Option<&str>) {
    let mut processes = match filter {
        Some(needle) => procs::find_by_name(needle),
        None => procs::list_processes(),
    };
    processes.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.pid.cmp(&b.pid)));
    println!("{:>8}  ИМЯ", "PID");
    for p in &processes {
        println!("{:>8}  {}", p.pid, p.name);
    }
    println!("\nвсего: {}", processes.len());
}

fn report_sweep(sweep: &etw::OrphanSweep) {
    if let Some(code) = sweep.query_error {
        eprintln!(
            "[cleanup] перечислить сессии не удалось: {}. Без прав администратора это ожидаемо; \
             проверьте вручную: logman query -ets",
            Win32Error(code)
        );
        return;
    }
    eprintln!(
        "[cleanup] сессий в системе: {}, осиротевших «{SWEEP_PREFIX}*» остановлено: {}",
        sweep.total_sessions,
        sweep.stopped.len()
    );
    for name in &sweep.stopped {
        eprintln!("[cleanup]   остановлена {name}");
    }
    for (name, code) in &sweep.failed {
        eprintln!("[cleanup]   НЕ остановлена {name}: {}", Win32Error(*code));
    }
}

fn resolve_target(target: &Target) -> Result<Option<procs::ProcessInfo>, String> {
    match target {
        Target::All => Ok(None),
        Target::Pid(pid) => {
            let name = procs::name_by_pid().get(pid).cloned();
            match name {
                Some(name) => Ok(Some(procs::ProcessInfo { pid: *pid, name })),
                // Процесс мог не найтись из-за прав на снимок, а не из-за отсутствия.
                // Для спайка это не повод отказываться от замера.
                None => Ok(Some(procs::ProcessInfo { pid: *pid, name: "?".to_string() })),
            }
        }
        Target::Name(needle) => {
            let found = procs::find_by_name(needle);
            match found.len() {
                0 => Err(format!("процесс с именем, содержащим «{needle}», не найден")),
                1 => Ok(Some(found.into_iter().next().expect("длина проверена"))),
                _ => {
                    let listing = found
                        .iter()
                        .map(|p| format!("  {:>8}  {}", p.pid, p.name))
                        .collect::<Vec<_>>()
                        .join("\n");
                    Err(format!(
                        "под «{needle}» подходит несколько процессов — укажите --pid:\n{listing}"
                    ))
                }
            }
        }
        Target::Foreground(delay) => {
            eprintln!("переключитесь в измеряемое приложение — цель будет взята из окна в фокусе");
            for remaining in (1..=*delay).rev() {
                eprintln!("  {remaining}...");
                std::thread::sleep(Duration::from_secs(1));
            }
            match procs::foreground_process() {
                Some((info, title)) => {
                    eprintln!("цель из фокуса: {} (pid {}) — «{}»", info.name, info.pid, title);
                    Ok(Some(info))
                }
                None => Err("не удалось определить окно в фокусе".to_string()),
            }
        }
    }
}

fn run(args: Args) -> ExitCode {
    let target = match resolve_target(&args.target) {
        Ok(target) => target,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    // Ставим аварийную уборку до создания сессии: иначе Ctrl+C в первые миллисекунды
    // оставит сироту.
    install_emergency_cleanup();

    let session_name = format!("{SESSION_PREFIX}{}", unsafe { GetCurrentProcessId() });
    report_sweep(&sweep_orphans(&session_name));

    let mut session = match Session::start(&session_name, args.buffers) {
        Ok(session) => session,
        Err(error @ StartError::AccessDenied) => {
            eprintln!("не удалось создать ETW-сессию: {error}");
            return ExitCode::from(EXIT_ACCESS_DENIED);
        }
        Err(error) => {
            eprintln!("не удалось создать ETW-сессию: {error}");
            return ExitCode::from(EXIT_ERROR);
        }
    };
    eprintln!("сессия: {}", session.name());

    // Провайдеры включаются ДО OpenTrace. Порядок проверен экспериментально: при включении
    // после запуска ProcessTrace потребителю не доставалось ни одного события.
    let chosen: &[&etw::Provider] = match args.providers {
        ProviderChoice::Both => &[&PROVIDER_DXGI, &PROVIDER_D3D9],
        ProviderChoice::DxgiOnly => &[&PROVIDER_DXGI],
        ProviderChoice::D3d9Only => &[&PROVIDER_D3D9],
    };
    let mut enabled: Vec<&str> = Vec::new();
    for provider in chosen {
        match session.enable_provider(provider, args.level) {
            Ok(()) => enabled.push(provider.label),
            // Один упавший провайдер не обесценивает замер по второму — сообщаем и идём дальше.
            Err(code) => {
                eprintln!("провайдер {} не включён: {}", provider.label, Win32Error(code))
            }
        }
    }
    if enabled.is_empty() {
        eprintln!("не включён ни один провайдер — замер бессмыслен");
        return ExitCode::from(EXIT_ERROR);
    }
    eprintln!("провайдеры: {}", enabled.join(", "));

    let qpc_freq = qpc_frequency();
    let collector = Arc::new(Collector::new(
        target.as_ref().map(|t| t.pid),
        qpc_freq,
        args.dxgi_id,
        args.d3d9_id,
        args.keep_test_presents,
    ));

    // OpenTrace и ProcessTrace выполняются в ОДНОМ потоке — том, который потом блокируется
    // на чтении. Регистрация realtime-потребителя привязана к вызывающему потоку, и при
    // открытии трейса в другом потоке ProcessTrace честно блокируется до остановки сессии,
    // но не получает ни одного буфера: всё уходит в EventsLost.
    //
    // Клон Arc уезжает в поток вместе с потребителем: callback держит на коллектор сырой
    // указатель, и коллектор обязан жить, пока жив поток, даже если main уже ушёл дальше.
    let collector_for_thread = Arc::clone(&collector);
    let (open_tx, open_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let thread_session_name = session_name.clone();
    let opened_at = Instant::now();
    let worker = std::thread::spawn(move || {
        let consumer = Consumer::open(
            &thread_session_name,
            Some(event_record_callback),
            Some(buffer_callback),
            Arc::as_ptr(&collector_for_thread) as *mut std::ffi::c_void,
        );
        let consumer = match consumer {
            Ok(consumer) => {
                let _ = open_tx.send(Ok(()));
                consumer
            }
            Err(code) => {
                let _ = open_tx.send(Err(code));
                return;
            }
        };
        let status = consumer.process();
        // Момент возврата важнее самого статуса: ProcessTrace обязан блокировать поток до
        // остановки сессии. Мгновенный возврат с ERROR_SUCCESS означает, что потребитель не
        // подключился, и все события уйдут в EventsLost.
        let (buffers_read, events_lost) = consumer.counters();
        eprintln!(
            "[consumer] ProcessTrace вернулся через {:.2} с, статус {}; BuffersRead {buffers_read}, EventsLost {events_lost}",
            opened_at.elapsed().as_secs_f64(),
            Win32Error(status)
        );
        let _ = done_tx.send(status);
        drop(collector_for_thread);
    });

    match open_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(code)) => {
            eprintln!("OpenTrace не удался: {}", Win32Error(code));
            return ExitCode::from(EXIT_ERROR);
        }
        Err(_) => {
            eprintln!("поток потребителя не отозвался за 10 с");
            return ExitCode::from(EXIT_ERROR);
        }
    }

    let started = Instant::now();
    match target.as_ref() {
        Some(t) => eprintln!(
            "измеряю {} (pid {}){}",
            t.name,
            t.pid,
            if args.seconds == 0 {
                ", до Ctrl+C".to_string()
            } else {
                format!(", {} с", args.seconds)
            }
        ),
        None => eprintln!("режим --all: цель не задана, считаю события по всем процессам"),
    }

    let mut last_frames = 0u64;
    let mut next_tick = Duration::from_secs(1);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        if stop_requested() {
            eprintln!("получен Ctrl+C — останавливаю сессию");
            break;
        }
        let elapsed = started.elapsed();
        if args.seconds > 0 && elapsed >= Duration::from_secs(args.seconds) {
            break;
        }
        // Раз в секунду показываем живой счёт: молчащий поток здесь видно сразу, а не
        // в конце замера.
        if elapsed >= next_tick {
            next_tick = elapsed + Duration::from_secs(1);
            let frames = collector.frames_so_far();
            eprintln!(
                "  [{:>4.0} с] событий всего {}, у цели {}, кадров {} (+{})",
                elapsed.as_secs_f64(),
                collector.total_events(),
                collector.target_events(),
                frames,
                frames.saturating_sub(last_frames)
            );
            last_frames = frames;
        }
    }
    let elapsed = started.elapsed();

    // Остановка сессии — единственный способ разблокировать ProcessTrace (PLAN.md §2.8).
    let trace_stats = session.stop();
    request_stop();

    match done_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(status) if status == ERROR_SUCCESS => {}
        Ok(status) => eprintln!("ProcessTrace завершился с {}", Win32Error(status)),
        Err(_) => eprintln!(
            "ProcessTrace не вернулся за 10 с. Сессия уже остановлена, сироты не будет, \
             но отчёт может быть неполным"
        ),
    }
    let _ = worker.join();

    report(&args, target.as_ref(), &collector, trace_stats, elapsed, qpc_freq)
}

// --- отчёт ---------------------------------------------------------------------------------

fn report(
    args: &Args,
    target: Option<&procs::ProcessInfo>,
    collector: &Collector,
    trace_stats: Option<etw::TraceStats>,
    elapsed: Duration,
    qpc_freq: i64,
) -> ExitCode {
    let state = collector.lock();
    let seconds = elapsed.as_secs_f64().max(1e-9);

    println!("\n=== P0: собственный ETW-потребитель кадров ===");
    println!("длительность      : {:.1} с", seconds);
    println!("частота QPC       : {qpc_freq} Гц");
    println!("событий всего     : {}", collector.total_events());

    println!("\n--- здоровье ETW ---");
    match trace_stats {
        Some(stats) => {
            println!(
                "EventsLost {}, RealTimeBuffersLost {}, LogBuffersLost {}, BuffersWritten {}, \
                 буферов {}",
                stats.events_lost,
                stats.real_time_buffers_lost,
                stats.log_buffers_lost,
                stats.buffers_written,
                stats.number_of_buffers
            );
            if stats.events_lost > 0 || stats.real_time_buffers_lost > 0 {
                println!(
                    "ВНИМАНИЕ: потери событий. Это ровно тот признак, за которым стоит \
                     осиротевшая сессия (PLAN.md §2.1) — проверьте logman query -ets"
                );
            }
        }
        None => println!("статистика сессии недоступна: остановку выполнил аварийный путь"),
    }
    // Второй, независимый источник тех же чисел: их сообщает сам потребитель на каждом
    // доставленном буфере. Расхождение с итогом сессии само по себе диагностично.
    println!(
        "по данным потребителя: буферов прочитано {}, событий потеряно {}",
        collector.buffers_read(),
        collector.events_lost()
    );

    // --- события ---
    if let Some(target) = target {
        println!("\n--- события цели {} (pid {}) ---", target.name, target.pid);
        print_event_table(state.events.iter().filter(|(k, _)| k.pid == target.pid), seconds);

        let foreign: u64 = state
            .events
            .iter()
            .filter(|(k, _)| k.pid != target.pid)
            .map(|(_, count)| *count)
            .sum();
        if foreign > 0 {
            println!("событий от других процессов: {foreign} (они отфильтрованы)");
        }
    } else {
        println!("\n--- процессы, от которых приходят события (топ {}) ---", args.top);
        print_by_process(&state, args.top, seconds);
    }

    // --- кадры ---
    if target.is_none() {
        println!(
            "\nрежим --all: frametime не считается. Если нужная игра есть в списке выше — \
             перезапустите с --pid её PID."
        );
        return ExitCode::SUCCESS;
    }
    let target = target.expect("ветка --all обработана выше");

    let dominant = state
        .chains
        .iter()
        .max_by_key(|(_, chain)| chain.presents)
        .map(|(id, chain)| (*id, chain));

    let Some((chain_id, chain)) = dominant else {
        println!("\n--- кадры ---");
        println!("НИ ОДНОГО Present-события от цели.");
        println!("Что проверить по порядку:");
        println!(
            "  1. осиротевшие ETW-сессии: logman query -ets (PLAN.md §2.1) — самая частая причина;"
        );
        println!("  2. верны ли ID событий: смотрите таблицу событий выше, при нужде --dxgi-id/--d3d9-id;");
        println!(
            "  3. графический API цели: DXGI и D3D9 не видят Vulkan и OpenGL (PLAN.md §2.4) — \
             для них это ожидаемый результат, его и надо занести в матрицу."
        );
        print_coverage_row(args, target, None, 0, 0.0);
        return ExitCode::from(EXIT_NO_FRAMES);
    };

    let provider_label =
        chain.provider.map(|p| p.label()).unwrap_or_else(|| "?".to_string());
    println!("\n--- кадры ---");
    println!("цепочек обмена    : {}", state.chains.len());
    println!(
        "ведущая цепочка   : 0x{chain_id:X} ({provider_label}), Present-событий {}",
        chain.presents
    );
    if state.chains.len() > 1 {
        for (id, other) in state.chains.iter() {
            if *id != chain_id {
                println!("  прочая цепочка  : 0x{id:X}, Present-событий {}", other.presents);
            }
        }
    }
    if state.presents_without_chain > 0 {
        println!(
            "без адреса цепочки: {} (payload короче 8 байт)",
            state.presents_without_chain
        );
    }
    if state.filtered_test_presents > 0 {
        println!(
            "отброшено тестовых : {} (DXGI_PRESENT_TEST — кадр на экран не выводится)",
            state.filtered_test_presents
        );
    }
    if !state.present_flags.is_empty() {
        let breakdown: Vec<String> = state
            .present_flags
            .iter()
            .map(|(flags, count)| format!("0x{flags:X}×{count}"))
            .collect();
        println!("флаги Present     : {}", breakdown.join(", "));
    }

    let Some(frame_stats) = stats::compute(&chain.frames) else {
        println!("frametime посчитать не из чего: Present-события есть, дельт нет");
        print_coverage_row(args, target, Some(&provider_label), 0, 0.0);
        return ExitCode::from(EXIT_NO_FRAMES);
    };

    println!("кадров (дельт)    : {}", frame_stats.accepted);
    if frame_stats.rejected > 0 {
        println!("отброшено значений: {}", frame_stats.rejected);
    }
    println!("средний FPS       : {:.1}", frame_stats.avg_fps);
    println!(
        "frametime, мс     : сред. {:.2}, мин. {:.2}, макс. {:.2}",
        frame_stats.avg_frametime_ms, frame_stats.min_frametime_ms, frame_stats.max_frametime_ms
    );
    match frame_stats.low_1_percent_fps {
        Some(value) => println!("1 % low, FPS      : {value:.1}"),
        None => println!(
            "1 % low, FPS      : выборка мала (нужно {} кадров)",
            stats::MIN_SAMPLES_1_PERCENT
        ),
    }
    match frame_stats.low_0_1_percent_fps {
        Some(value) => println!("0.1 % low, FPS    : {value:.1}"),
        None => println!(
            "0.1 % low, FPS    : выборка мала (нужно {} кадров)",
            stats::MIN_SAMPLES_0_1_PERCENT
        ),
    }

    if let Some(path) = &args.csv {
        match write_csv(path, &chain.frames) {
            Ok(()) => println!("frametime выгружены: {path}"),
            Err(error) => eprintln!("не удалось записать {path}: {error}"),
        }
    }

    print_coverage_row(
        args,
        target,
        Some(&provider_label),
        frame_stats.accepted,
        frame_stats.avg_fps,
    );
    ExitCode::SUCCESS
}

fn print_event_table<'a>(
    events: impl Iterator<Item = (&'a collect::EventKey, &'a u64)>,
    seconds: f64,
) {
    let mut rows: Vec<_> = events.collect();
    rows.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    if rows.is_empty() {
        println!("(пусто)");
        return;
    }
    println!("{:<12} {:>5} {:>4} {:>4} {:>10} {:>10}", "ПРОВАЙДЕР", "ID", "ВЕР", "ОП", "СОБЫТИЙ", "В СЕК");
    for (key, count) in rows {
        println!(
            "{:<12} {:>5} {:>4} {:>4} {:>10} {:>10.1}",
            key.provider.label(),
            key.id,
            key.version,
            key.opcode,
            count,
            *count as f64 / seconds
        );
    }
}

/// Сколько событий пришло от процесса: всего и в разбивке по (провайдер, ID события).
type PerProcess = BTreeMap<u32, (u64, BTreeMap<(ProviderKey, u16), u64>)>;

fn print_by_process(state: &collect::State, top: usize, seconds: f64) {
    let mut per_pid: PerProcess = BTreeMap::new();
    for (key, count) in &state.events {
        let entry = per_pid.entry(key.pid).or_default();
        entry.0 += count;
        *entry.1.entry((key.provider, key.id)).or_insert(0) += count;
    }

    let mut rows: Vec<_> = per_pid.into_iter().collect();
    rows.sort_by_key(|(_, (total, _))| std::cmp::Reverse(*total));

    let names = procs::name_by_pid();
    println!("{:>8}  {:<28} {:>10} {:>9}  СОБЫТИЯ", "PID", "ПРОЦЕСС", "СОБЫТИЙ", "В СЕК");
    for (pid, (total, breakdown)) in rows.into_iter().take(top) {
        let name = names.get(&pid).cloned().unwrap_or_else(|| "?".to_string());
        let detail: Vec<String> = breakdown
            .iter()
            .map(|((provider, id), count)| format!("{} {id}×{count}", provider.label()))
            .collect();
        println!(
            "{pid:>8}  {:<28} {total:>10} {:>9.1}  {}",
            truncate(&name, 28),
            total as f64 / seconds,
            detail.join(", ")
        );
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit.saturating_sub(1)).chain(std::iter::once('…')).collect()
}

/// Готовая строка для матрицы покрытия — её остаётся вставить в COVERAGE.md.
/// Матрица и есть результат P0, ради которого спайк написан.
fn print_coverage_row(
    args: &Args,
    target: &procs::ProcessInfo,
    provider: Option<&str>,
    frames: usize,
    avg_fps: f64,
) {
    let api = args.label.as_deref().unwrap_or("?");
    let source = match provider {
        Some(label) => label.to_string(),
        None => "—".to_string(),
    };
    let visible = if frames > 0 { "да" } else { "нет" };
    let fps = if frames > 0 { format!("{avg_fps:.1}") } else { "—".to_string() };
    println!("\n--- строка для COVERAGE.md ---");
    println!("| {api} | {} | {source} | {frames} | {fps} | {visible} |", target.name);
}

fn write_csv(path: &str, frames: &[f32]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut out = std::io::BufWriter::new(file);
    writeln!(out, "index,frametime_ms")?;
    for (index, value) in frames.iter().enumerate() {
        writeln!(out, "{index},{value:.4}")?;
    }
    out.flush()
}

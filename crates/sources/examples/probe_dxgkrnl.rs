//! Пробник: какие события `Microsoft-Windows-DxgKrnl` идут на каждый кадр OpenGL-игры, которая
//! выводит через GDI-копию (PLAN.md §2.16).
//!
//! PresentMon в режиме `Composed: Copy with GPU GDI` видит лишь часть кадров, собственный ETW
//! OpenGL не видит вовсе. Ищем событие ядра, которое приходит ровно на каждый кадр.
//!
//! Запускать **от администратора**, игра — в окне, счётчик кадров игры на виду:
//!
//! ```powershell
//! cargo run -p mh-sources --example probe_dxgkrnl --release -- --name fury.exe --seconds 20
//! cargo run -p mh-sources --example probe_dxgkrnl --release -- --name fury.exe --level 4 --keywords 0x1
//! ```
//!
//! Раз в 5 с печатается частота каждого события в секунду — отдельно для процесса игры и для
//! всех процессов: при GDI-копии часть работы может записываться от имени DWM или ядра.
//!
//! `--level <0..5>` и `--keywords <0x…>` сужают поток: по умолчанию включено всё (уровень 5,
//! все ключевые слова), а это сотни тысяч событий в секунду — для постоянной работы не годится.
//! `--only 184,166` включает фильтр по номерам событий на стороне ETW — проверка, что поток
//! можно сузить до одного события на кадр.
//!
//! Первый прогон на Ion Fury (всё включено): кадрам игры соответствуют события 184, 166, 215,
//! 43, 107, 108, 167 — их частота совпала со счётчиком FPS игры.

#[cfg(not(windows))]
fn main() {
    eprintln!("пробник требует Windows");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    probe::run()
}

#[cfg(windows)]
mod probe {
    use std::collections::HashMap;
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use mh_core::session_name;
    use mh_platform::console::{install_ctrl_handler, stop_requested};
    use mh_platform::etw::{Consumer, EventInfo, EventSink, Provider, Session, sweep_orphans};
    use mh_platform::process::list_processes;

    const DXGKRNL_GUID: u128 = 0x802EC45A_1E99_4B83_9920_87C98277BA9D;
    /// События, частота которых совпала с FPS игры в первом прогоне.
    const FRAME_CANDIDATES: [u16; 3] = [184, 166, 215];
    const REPORT_EVERY: Duration = Duration::from_secs(5);
    const TOP: usize = 15;

    type Key = (u16, u8); // (id, opcode)

    /// Посекундная картина события 184 у игры: сколько за секунду и какие интервалы.
    #[derive(Default)]
    struct Timeline {
        last_qpc: Option<i64>,
        intervals_ms: Vec<f64>,
    }

    struct Counter {
        target_pid: u32,
        qpc_frequency: f64,
        timeline: Mutex<Timeline>,
        target: Mutex<HashMap<Key, u64>>,
        all: Mutex<HashMap<Key, u64>>,
        lost: AtomicU32,
    }

    impl EventSink for Counter {
        fn on_event(&self, event: &EventInfo<'_>) {
            let key = (event.event_id, event.opcode);
            *self.all.lock().unwrap().entry(key).or_default() += 1;
            if event.process_id == self.target_pid {
                *self.target.lock().unwrap().entry(key).or_default() += 1;
                if event.event_id == 184 {
                    let mut timeline = self.timeline.lock().unwrap();
                    if let Some(last) = timeline.last_qpc {
                        let ms = (event.timestamp_qpc - last) as f64 * 1_000.0 / self.qpc_frequency;
                        timeline.intervals_ms.push(ms);
                    }
                    timeline.last_qpc = Some(event.timestamp_qpc);
                }
            }
        }

        fn on_buffer(&self, _buffers_read: u32, events_lost: u32) -> bool {
            self.lost.store(events_lost, Ordering::Relaxed);
            true
        }
    }

    fn take(map: &Mutex<HashMap<Key, u64>>) -> Vec<(Key, u64)> {
        let mut rows: Vec<(Key, u64)> = map.lock().unwrap().drain().collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.1));
        rows
    }

    fn print(title: &str, rows: &[(Key, u64)], seconds: f64) {
        println!("  {title}:");
        if rows.is_empty() {
            println!("    —");
        }
        for ((id, opcode), count) in rows.iter().take(TOP) {
            println!(
                "    id {id:>4} opcode {opcode:>3}: {:>9.1}/с  ({count})",
                *count as f64 / seconds
            );
        }
    }

    /// Средний и крайние интервалы события 184 — FPS по нему и разброс.
    fn print_timeline(timeline: &Mutex<Timeline>) {
        let intervals = std::mem::take(&mut timeline.lock().unwrap().intervals_ms);
        if intervals.is_empty() {
            println!("  184: интервалов нет");
            return;
        }
        let mut sorted = intervals.clone();
        sorted.sort_by(f64::total_cmp);
        let mean = intervals.iter().sum::<f64>() / intervals.len() as f64;
        let pick = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
        println!(
            "  184: {} интервалов, средний {:.2} мс ({:.1} FPS), p1 {:.2}, медиана {:.2}, p99 {:.2},              макс {:.2}; короче 1 мс: {}",
            intervals.len(),
            mean,
            1_000.0 / mean,
            pick(0.01),
            pick(0.5),
            pick(0.99),
            sorted[sorted.len() - 1],
            intervals.iter().filter(|ms| **ms < 1.0).count()
        );
    }

    fn argument(name: &str) -> Option<String> {
        std::env::args().skip_while(|arg| arg != name).nth(1)
    }

    pub fn run() -> ExitCode {
        let seconds: u64 = argument("--seconds").and_then(|v| v.parse().ok()).unwrap_or(20);
        let level: u8 = argument("--level").and_then(|v| v.parse().ok()).unwrap_or(5);
        let keywords = argument("--keywords")
            .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
            .unwrap_or(u64::MAX);
        let target_pid = match (argument("--pid"), argument("--name")) {
            (Some(pid), _) => pid.parse().ok(),
            (None, Some(name)) => list_processes()
                .into_iter()
                .find(|(_, exe)| exe.eq_ignore_ascii_case(&name))
                .map(|(pid, _)| pid),
            (None, None) => None,
        };
        let Some(target_pid) = target_pid else {
            eprintln!("цель не найдена: --pid <PID> или --name <ИМЯ.exe> запущенного процесса");
            return ExitCode::from(2);
        };
        let only: Vec<u16> = argument("--only")
            .map(|list| list.split(',').filter_map(|id| id.trim().parse().ok()).collect())
            .unwrap_or_default();
        let provider = Provider {
            label: "Microsoft-Windows-DxgKrnl",
            guid: DXGKRNL_GUID,
            level,
            any_keyword: keywords,
        };

        let session = session_name(std::process::id());
        println!("[гигиена] {}", sweep_orphans(&session).describe());
        install_ctrl_handler(&session);
        let mut trace = match Session::start(&session) {
            Ok(trace) => trace,
            Err(error) => {
                eprintln!("сессия не создана: {error:?} (нужны права администратора)");
                return ExitCode::from(1);
            }
        };
        let enabled = if only.is_empty() {
            trace.enable_provider(&provider, None)
        } else {
            trace.enable_provider_filtered(&provider, &only)
        };
        if let Err(error) = enabled {
            eprintln!("DxgKrnl не включён: {error}");
            return ExitCode::from(1);
        }

        let counter = Arc::new(Counter {
            target_pid,
            qpc_frequency: mh_platform::qpc_frequency() as f64,
            timeline: Mutex::new(Timeline::default()),
            target: Mutex::new(HashMap::new()),
            all: Mutex::new(HashMap::new()),
            lost: AtomicU32::new(0),
        });
        let consumer =
            match Consumer::open_with_sink(&session, Arc::clone(&counter) as Arc<dyn EventSink>) {
                Ok(consumer) => consumer,
                Err(error) => {
                    eprintln!("потребитель не открыт: {error}");
                    return ExitCode::from(1);
                }
            };
        let reader = std::thread::spawn(move || {
            consumer.process();
        });

        println!(
            "цель pid {target_pid}; {seconds} с; уровень {level}, ключевые слова {keywords:#x}"
        );
        if !only.is_empty() {
            println!("фильтр по номерам событий: {only:?}");
        }
        println!("сравнивайте частоты со счётчиком кадров игры\n");
        let started = Instant::now();
        let mut last = Instant::now();
        while started.elapsed() < Duration::from_secs(seconds) && !stop_requested() {
            std::thread::sleep(Duration::from_millis(100));
            if last.elapsed() >= REPORT_EVERY {
                let span = last.elapsed().as_secs_f64();
                last = Instant::now();
                let target = take(&counter.target);
                let all = take(&counter.all);
                let total: u64 = all.iter().map(|(_, count)| count).sum();
                println!(
                    "[{:>3} с] потеряно событий: {}, всего событий: {:.0}/с",
                    started.elapsed().as_secs(),
                    counter.lost.load(Ordering::Relaxed),
                    total as f64 / span
                );
                for id in FRAME_CANDIDATES {
                    let count: u64 =
                        target.iter().filter(|((row, _), _)| *row == id).map(|(_, c)| c).sum();
                    println!("  кандидат {id}: {:.1}/с у игры", count as f64 / span);
                }
                print_timeline(&counter.timeline);
                print("процесс игры", &target, span);
                print("все процессы", &all, span);
                println!();
            }
        }

        let stats = trace.stop();
        let _ = reader.join();
        println!("сессия остановлена: {stats:?}");
        ExitCode::SUCCESS
    }
}

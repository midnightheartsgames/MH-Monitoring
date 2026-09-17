//! Сервер снимков: движок и именованный канал (PLAN.md §6/P6).
//!
//! Один и тот же код работает внутри службы Windows и в отладочном режиме
//! `MH-Monitoring-Service.exe --serve`. UI выбирает цель сам и присылает её; сервер меряет и рассылает снимки.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mh_core::{FpsReason, TargetResolution};
use mh_engine::{Engine, EngineConfig, extract_presentmon};
use mh_ipc::{PIPE_NAME, PROTOCOL_VERSION, ToClient, ToService, read_message, write_message};
use mh_platform::diag;
use mh_platform::pipe::{PipeListener, PipeStream, SERVICE_PIPE_SDDL};

/// Как часто клиенту уходит снимок. UI обновляется с той же частотой, что и движок.
const SNAPSHOT_EVERY: Duration = Duration::from_millis(250);
/// Как часто клиентский поток проверяет входящие сообщения.
const CLIENT_TICK: Duration = Duration::from_millis(20);

/// Работает, пока не поднят `stop`. Блокирует.
pub fn run(stop: Arc<AtomicBool>) -> std::io::Result<()> {
    // Вложенный PresentMon — только из папки профиля, куда обычный пользователь не пишет: служба
    // запускает его от SYSTEM. Свой путь пользователя здесь не принимается вовсе.
    let bin = crate::local_dir().join("bin");
    let presentmon = diag::timed("PresentMon разложен", || extract_presentmon(&bin))?;
    let engine = Arc::new(diag::timed("движок запущен", || {
        Engine::start(EngineConfig { presentmon, presentmon_override: None }, || {})
    }));

    let mut listener = PipeListener::new(PIPE_NAME, Some(SERVICE_PIPE_SDDL))?;
    let clients = Arc::new(AtomicUsize::new(0));
    let mut threads = Vec::new();
    diag::log(format!("канал {PIPE_NAME} открыт"));

    while !stop.load(Ordering::Relaxed) {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(error) => {
                diag::log(format!("канал: {error}"));
                // Имя занято чужим процессом или канал не создаётся — повторять бессмысленно.
                if threads.is_empty() && error.kind() == std::io::ErrorKind::PermissionDenied {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        clients.fetch_add(1, Ordering::SeqCst);
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        let stop = Arc::clone(&stop);
        threads.push(std::thread::spawn(move || {
            serve_client(stream, &engine, &stop);
            // Последний клиент ушёл — мерить некого: PresentMon не должен работать впустую.
            if clients.fetch_sub(1, Ordering::SeqCst) == 1 {
                engine.set_target(no_target());
            }
        }));
        threads.retain(|thread| !thread.is_finished());
    }

    for thread in threads {
        let _ = thread.join();
    }
    diag::timed("движок остановлен", || drop(engine));
    Ok(())
}

/// Будит [`run`], если он ждёт клиента, — после подъёма `stop`.
pub fn wake() {
    PipeListener::wake(PIPE_NAME);
}

fn serve_client(mut stream: PipeStream, engine: &Engine, stop: &AtomicBool) {
    let mut greeted = false;
    let mut next_snapshot = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match stream.available() {
            Ok(0) => {}
            Ok(_) => match read_message::<ToService>(&mut stream) {
                Ok(Some(ToService::Hello { protocol, client_pid })) => {
                    if protocol != PROTOCOL_VERSION {
                        let reason = format!(
                            "UI говорит на протоколе {protocol}, служба — на {PROTOCOL_VERSION}"
                        );
                        diag::log(format!("клиент {client_pid} отклонён: {reason}"));
                        let _ = write_message(&mut stream, &ToClient::Refused { reason });
                        return;
                    }
                    diag::log(format!("клиент {client_pid} подключён"));
                    let hello = ToClient::Hello {
                        protocol: PROTOCOL_VERSION,
                        service_version: env!("CARGO_PKG_VERSION").to_string(),
                    };
                    if write_message(&mut stream, &hello).is_err() {
                        return;
                    }
                    greeted = true;
                }
                Ok(Some(ToService::Target(target))) if greeted => engine.set_target(target),
                // Цель до приветствия — нарушение протокола.
                Ok(Some(ToService::Target(_))) | Ok(None) | Err(_) => return,
            },
            Err(_) => return,
        }

        if greeted && Instant::now() >= next_snapshot {
            next_snapshot = Instant::now() + SNAPSHOT_EVERY;
            let snapshot = ToClient::Snapshot(Box::new(engine.snapshot()));
            if write_message(&mut stream, &snapshot).is_err() {
                return;
            }
        }
        std::thread::sleep(CLIENT_TICK);
    }
}

fn no_target() -> TargetResolution {
    TargetResolution::Unresolved { reason: FpsReason::NoTarget, detail: None }
}

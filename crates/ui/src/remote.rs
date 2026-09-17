//! Клиент службы: получает снимки, отправляет цель, переподключается сам (PLAN.md §6/P6).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;
use mh_core::{Snapshot, TargetResolution};
use mh_ipc::{PIPE_NAME, PROTOCOL_VERSION, ToClient, ToService, read_message, write_message};
use mh_platform::pipe::PipeStream;

use crate::diag;

const TICK: Duration = Duration::from_millis(20);
const RECONNECT_EVERY: Duration = Duration::from_secs(1);
/// Цель уходит заново и без перемен — служба могла перезапуститься и её забыть.
const TARGET_REFRESH: Duration = Duration::from_secs(1);

/// Состояние соединения — для строки в HUD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Connected { service_version: String },
    Refused(String),
}

#[derive(Default)]
struct Shared {
    snapshot: Mutex<Option<Snapshot>>,
    target: Mutex<Option<TargetResolution>>,
    link: Mutex<Option<Link>>,
    stop: AtomicBool,
}

pub struct RemoteEngine {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl RemoteEngine {
    pub fn start(ctx: egui::Context) -> RemoteEngine {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("mh-remote".into())
            .spawn(move || run(&worker, &ctx))
            .ok();
        RemoteEngine { shared, thread }
    }

    /// Можно ли подключиться прямо сейчас — служба запущена и слушает.
    pub fn service_available() -> bool {
        PipeStream::connect(PIPE_NAME, 0).is_ok()
    }

    /// Последний снимок службы. Пока соединения нет — пустой: устаревшие цифры хуже прочерков.
    pub fn snapshot(&self) -> Snapshot {
        let connected = matches!(self.link(), Link::Connected { .. });
        if !connected {
            return Snapshot::default();
        }
        self.shared.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
    }

    pub fn set_target(&self, target: TargetResolution) {
        *self.shared.target.lock().unwrap_or_else(|e| e.into_inner()) = Some(target);
    }

    pub fn link(&self) -> Link {
        self.shared
            .link
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or(Link::Connecting)
    }
}

impl Drop for RemoteEngine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn set_link(shared: &Shared, link: Link) {
    *shared.link.lock().unwrap_or_else(|e| e.into_inner()) = Some(link);
}

fn run(shared: &Shared, ctx: &egui::Context) {
    while !shared.stop.load(Ordering::Relaxed) {
        match PipeStream::connect(PIPE_NAME, 500) {
            Ok(stream) => {
                session(shared, ctx, stream);
                if !matches!(
                    *shared.link.lock().unwrap_or_else(|e| e.into_inner()),
                    Some(Link::Refused(_))
                ) {
                    set_link(shared, Link::Connecting);
                }
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
            Err(_) => set_link(shared, Link::Connecting),
        }
        let deadline = Instant::now() + RECONNECT_EVERY;
        while Instant::now() < deadline && !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(TICK);
        }
    }
}

/// Одно соединение — до разрыва или остановки.
fn session(shared: &Shared, ctx: &egui::Context, mut stream: PipeStream) {
    let hello = ToService::Hello { protocol: PROTOCOL_VERSION, client_pid: std::process::id() };
    if write_message(&mut stream, &hello).is_err() {
        return;
    }
    let mut sent_target: Option<TargetResolution> = None;
    let mut next_refresh = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        match stream.available() {
            Ok(0) => {}
            Ok(_) => match read_message::<ToClient>(&mut stream) {
                Ok(Some(ToClient::Hello { service_version, .. })) => {
                    diag::log(format!("служба {service_version} подключена"));
                    set_link(shared, Link::Connected { service_version });
                    // Новая служба цели не знает — отправить сразу.
                    sent_target = None;
                }
                Ok(Some(ToClient::Snapshot(snapshot))) => {
                    *shared.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Some(*snapshot);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                Ok(Some(ToClient::Refused { reason })) => {
                    diag::log(format!("служба отказала: {reason}"));
                    set_link(shared, Link::Refused(reason));
                    return;
                }
                Ok(None) | Err(_) => return,
            },
            Err(_) => return,
        }

        let target = shared.target.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(target) = target
            && (sent_target.as_ref() != Some(&target) || Instant::now() >= next_refresh)
        {
            if write_message(&mut stream, &ToService::Target(target.clone())).is_err() {
                return;
            }
            sent_target = Some(target);
            next_refresh = Instant::now() + TARGET_REFRESH;
        }
        std::thread::sleep(TICK);
    }
}

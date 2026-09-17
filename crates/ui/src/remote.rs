//! Клиент службы: получает снимки, отправляет цель, переподключается сам (PLAN.md §6/P6).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;
use mh_core::{SensorOptions, Snapshot, TargetResolution};
use mh_ipc::{
    MIN_PROTOCOL_VERSION, PIPE_NAME, PROTOCOL_VERSION, ToClient, ToService, read_message,
    supports_sensor_options, write_message,
};
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
    /// `sensor_options` — служба понимает выбор видеокарты и интервал опроса. Старая служба не понимает: с ней клиент
    /// говорит на старом протоколе.
    Connected {
        service_version: String,
        sensor_options: bool,
    },
    Refused(String),
}

struct Shared {
    snapshot: Mutex<Option<Snapshot>>,
    target: Mutex<Option<TargetResolution>>,
    sensors: Mutex<SensorOptions>,
    link: Mutex<Option<Link>>,
    /// На каком протоколе здороваться. Старая служба отказывает новому — тогда клиент
    /// переходит на старый, пока служба не пропадёт (её могли обновить).
    protocol: AtomicU32,
    stop: AtomicBool,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            snapshot: Mutex::default(),
            target: Mutex::default(),
            sensors: Mutex::default(),
            link: Mutex::default(),
            protocol: AtomicU32::new(PROTOCOL_VERSION),
            stop: AtomicBool::default(),
        }
    }
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

    pub fn set_sensor_options(&self, options: SensorOptions) {
        *self.shared.sensors.lock().unwrap_or_else(|e| e.into_inner()) = options;
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
                let protocol = shared.protocol.load(Ordering::Relaxed);
                session(shared, ctx, stream);
                if shared.protocol.load(Ordering::Relaxed) != protocol {
                    // Перешли на старый протокол — сразу снова, без паузы переподключения.
                    continue;
                }
                if !matches!(
                    *shared.link.lock().unwrap_or_else(|e| e.into_inner()),
                    Some(Link::Refused(_))
                ) {
                    set_link(shared, Link::Connecting);
                }
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
            Err(_) => {
                set_link(shared, Link::Connecting);
                // Службы нет — может, её обновляют. В следующий раз снова новый протокол.
                shared.protocol.store(PROTOCOL_VERSION, Ordering::Relaxed);
            }
        }
        let deadline = Instant::now() + RECONNECT_EVERY;
        while Instant::now() < deadline && !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(TICK);
        }
    }
}

/// Одно соединение — до разрыва или остановки.
fn session(shared: &Shared, ctx: &egui::Context, mut stream: PipeStream) {
    let protocol = shared.protocol.load(Ordering::Relaxed);
    let hello = ToService::Hello { protocol, client_pid: std::process::id() };
    if write_message(&mut stream, &hello).is_err() {
        return;
    }
    let sensor_options_supported = supports_sensor_options(protocol);
    let mut sent_target: Option<TargetResolution> = None;
    let mut sent_sensors: Option<SensorOptions> = None;
    let mut next_refresh = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        match stream.available() {
            Ok(0) => {}
            Ok(_) => match read_message::<ToClient>(&mut stream) {
                Ok(Some(ToClient::Hello { service_version, .. })) => {
                    diag::log(format!("служба {service_version} подключена, протокол {protocol}"));
                    set_link(
                        shared,
                        Link::Connected {
                            service_version,
                            sensor_options: sensor_options_supported,
                        },
                    );
                    // Новая служба ни цели, ни интервала не знает — отправить сразу.
                    sent_target = None;
                    sent_sensors = None;
                }
                Ok(Some(ToClient::Snapshot(snapshot))) => {
                    *shared.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Some(*snapshot);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                Ok(Some(ToClient::Refused { reason })) => {
                    if protocol > MIN_PROTOCOL_VERSION {
                        // Служба старше UI. Говорим на её протоколе, без новых сообщений.
                        diag::log(format!(
                            "служба отказала ({reason}) — протокол {MIN_PROTOCOL_VERSION}"
                        ));
                        shared.protocol.store(MIN_PROTOCOL_VERSION, Ordering::Relaxed);
                        return;
                    }
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

        let options = shared.sensors.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if sensor_options_supported && sent_sensors.as_ref() != Some(&options) {
            if write_message(&mut stream, &ToService::Sensors(options.clone())).is_err() {
                return;
            }
            sent_sensors = Some(options);
        }
        std::thread::sleep(TICK);
    }
}

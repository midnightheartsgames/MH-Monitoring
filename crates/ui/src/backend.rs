//! Откуда берутся снимки: служба или движок в этом процессе (PLAN.md §6/P6).
//!
//! Служба — основной путь: она работает от SYSTEM, и UI не нужны права администратора. Без неё
//! движок работает здесь же, как в P4–P5, — и честно показывает, чего без прав не видно.

use std::path::PathBuf;

use eframe::egui;
use mh_core::{SensorOptions, Snapshot, TargetResolution};
use mh_engine::{Engine, EngineConfig};

use crate::diag;
use crate::remote::{Link, RemoteEngine};

pub enum Backend {
    /// Свой движок; `bool` — установлена ли программа (тогда остановлена служба).
    Local(Engine, bool),
    Remote(RemoteEngine),
}

/// Что нужно, чтобы поднять движок в этом процессе.
#[derive(Clone)]
pub struct LocalConfig {
    pub presentmon: PathBuf,
    pub presentmon_override: Option<PathBuf>,
}

impl Backend {
    /// Служба, если она слушает; иначе — свой движок.
    pub fn start(ctx: &egui::Context, local: &LocalConfig) -> Backend {
        if RemoteEngine::service_available() {
            diag::log("снимки — от службы");
            Backend::Remote(RemoteEngine::start(ctx.clone()))
        } else {
            Backend::start_local(ctx, local)
        }
    }

    pub fn start_local(ctx: &egui::Context, local: &LocalConfig) -> Backend {
        let repaint = ctx.clone();
        let engine = diag::timed("движок в процессе запущен", || {
            Engine::start(
                EngineConfig {
                    presentmon: local.presentmon.clone(),
                    presentmon_override: local.presentmon_override.clone(),
                },
                move || repaint.request_repaint_of(egui::ViewportId::ROOT),
            )
        });
        Backend::Local(engine, crate::installer::installed_version().is_some())
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Backend::Local(..))
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Backend::Remote(..))
    }

    pub fn snapshot(&self) -> Snapshot {
        match self {
            Backend::Local(engine, _) => engine.snapshot(),
            Backend::Remote(remote) => remote.snapshot(),
        }
    }

    pub fn set_target(&self, target: TargetResolution) {
        match self {
            Backend::Local(engine, _) => engine.set_target(target),
            Backend::Remote(remote) => remote.set_target(target),
        }
    }

    pub fn set_sensor_options(&self, options: SensorOptions) {
        match self {
            Backend::Local(engine, _) => engine.set_sensor_options(options),
            Backend::Remote(remote) => remote.set_sensor_options(options),
        }
    }

    /// Применяется ли интервал опроса. Нет — только у службы старой версии.
    pub fn sensor_options_supported(&self) -> bool {
        match self {
            Backend::Local(..) => true,
            Backend::Remote(remote) => match remote.link() {
                Link::Connected { sensor_options, .. } => sensor_options,
                Link::Connecting | Link::Refused(_) => true,
            },
        }
    }

    /// Строка для HUD о связи со службой — если сказать есть что.
    pub fn note(&self) -> Option<String> {
        match self {
            // Установлено, а снимки свои — значит, служба остановлена. Иначе HUD говорил бы только
            // «нужны права администратора», и непонятно, что делать.
            Backend::Local(_, installed) => installed.then(|| {
                "служба MH Monitoring остановлена — запустите её в настройках, «Установка»"
                    .to_string()
            }),
            Backend::Remote(remote) => match remote.link() {
                Link::Connected { .. } => None,
                Link::Connecting => Some("служба MH Monitoring не отвечает — жду".to_string()),
                Link::Refused(reason) => Some(format!("служба отказала: {reason}")),
            },
        }
    }

    /// Для окна «О программе».
    pub fn describe(&self) -> String {
        match self {
            Backend::Local(..) => "движок в этом процессе".to_string(),
            Backend::Remote(remote) => match remote.link() {
                Link::Connected { service_version, sensor_options: true } => {
                    format!("служба {service_version}")
                }
                Link::Connected { service_version, sensor_options: false } => {
                    format!("служба {service_version} (старого протокола)")
                }
                Link::Connecting => "служба — нет связи".to_string(),
                Link::Refused(reason) => format!("служба отказала: {reason}"),
            },
        }
    }
}

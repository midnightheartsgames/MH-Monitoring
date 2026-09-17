//! Предпросмотр HUD в окне настроек.
//!
//! Рисует настоящий [`crate::hud::show`] по черновику настроек, но на выдуманных цифрах: живой
//! снимок без игры состоит из прочерков, а по прочеркам не видно, что включено.

use eframe::egui::Ui;
use mh_core::{
    CoreStats, CpuStats, FpsAvailability, FpsState, FrameStatistics, FrametimeGraph, GpuStats,
    MemoryStats, SectionHealth, SensorStatus, Snapshot, TargetProcess,
};

use crate::hud::{self, Extras, HudActions, SeenRows};
use crate::settings::Settings;

const GIB: u64 = 1 << 30;
/// Время в предпросмотре.
const DEMO_CLOCK: (u8, u8) = (21, 37);
/// Разрешение игры в предпросмотре.
pub const DEMO_RESOLUTION: (u32, u32) = (2560, 1440);
/// Сколько ядер у выдуманного процессора, пока настоящие не известны.
const DEMO_THREADS: usize = 16;

/// Снимок «идёт игра»: все строки с правдоподобными значениями.
pub fn demo_snapshot() -> Snapshot {
    let columns = (0..120)
        .map(|index| match index % 37 {
            0 => 14.5,
            _ => 6.9 + (index % 5) as f32 * 0.15,
        })
        .collect();
    Snapshot {
        gpu: GpuStats {
            health: SectionHealth::available(),
            name: Some("NVIDIA GeForce RTX".into()),
            load_percent: Some(74.0),
            temperature_c: Some(66.0),
            core_clock_mhz: Some(1_687.0),
            memory_clock_mhz: Some(7_001.0),
            vram_used_bytes: Some(7 * GIB + GIB / 2),
            vram_total_bytes: Some(16 * GIB),
            power_watts: Some(214.0),
            fan_rpm: Some(1_450.0),
            fan_percent: Some(38.0),
        },
        cpu: CpuStats {
            health: SectionHealth::available(),
            name: Some("AMD Ryzen 7".into()),
            load_percent: Some(38.0),
            temperature_c: Some(71.0),
            clock_mhz: Some(4_620.0),
            power_watts: Some(96.0),
            cores: demo_cores(&[0; DEMO_THREADS]),
        },
        memory: MemoryStats {
            health: SectionHealth::available(),
            used_bytes: Some(13 * GIB + GIB * 7 / 10),
            total_bytes: Some(32 * GIB),
            speed_mhz: Some(6_000),
            process_bytes: Some(4 * GIB + GIB * 3 / 10),
        },
        fps: FpsState {
            availability: FpsAvailability::Available,
            target: Some(TargetProcess::new(0, "game.exe", None)),
            presentation: Some("DXGI · Hardware: Independent Flip".into()),
            latency_ms: Some(27.0),
            statistics: FrameStatistics {
                current_fps: Some(143.0),
                average_fps: Some(142.0),
                low_1_percent_fps: Some(98.0),
                low_0_1_percent_fps: Some(75.0),
                current_frametime_ms: Some(7.1),
                sample_count: 5_000,
                rejected_count: 0,
            },
            ..FpsState::INITIAL
        },
        frametime_graph: FrametimeGraph { columns, ceiling_ms: 33.3 },
        hardware_status: SensorStatus::Available,
        timestamp_ms: 0,
    }
}

/// Выдуманные нагрузка и частоты для ядер с такими классами.
fn demo_cores(classes: &[u8]) -> Vec<CoreStats> {
    let top = classes.iter().copied().max().unwrap_or(0);
    classes
        .iter()
        .enumerate()
        .map(|(index, &class)| {
            let wave = ((index * 37) % 11) as f64;
            let performance = class == top;
            CoreStats {
                load_percent: Some(if performance { 30.0 + wave * 6.0 } else { 10.0 + wave * 3.0 }),
                clock_mhz: Some(if performance {
                    4_550.0 + wave * 12.0
                } else {
                    3_700.0 + wave * 8.0
                }),
                efficiency_class: class,
            }
        })
        .collect()
}

/// Устройство — настоящее, если система о нём сообщила: имена и число ядер в предпросмотре
/// должны быть те, что будут в HUD.
pub fn use_live_devices(demo: &mut Snapshot, live: &Snapshot) {
    if let Some(name) = &live.gpu.name {
        demo.gpu.name = Some(name.clone());
    }
    if let Some(name) = &live.cpu.name {
        demo.cpu.name = Some(name.clone());
    }
    let classes: Vec<u8> = live.cpu.cores.iter().map(|core| core.efficiency_class).collect();
    let demo_classes: Vec<u8> = demo.cpu.cores.iter().map(|core| core.efficiency_class).collect();
    if !classes.is_empty() && classes != demo_classes {
        demo.cpu.cores = demo_cores(&classes);
    }
}

/// HUD по черновику. Кнопки заголовка в предпросмотре ничего не делают.
pub fn show(ui: &mut Ui, snapshot: &Snapshot, settings: &Settings) {
    let mut seen = SeenRows::new();
    let mut actions = HudActions::default();
    let extras = Extras {
        notes: &[],
        resolution: Some(DEMO_RESOLUTION),
        clock: settings.overlay.show_clock.then_some(DEMO_CLOCK),
    };
    hud::show(ui, snapshot, settings, &mut seen, &extras, &mut actions);
}

//! Живая проверка датчиков железа (фаза P3).
//!
//! Раз в секунду печатает снимок так, как его увидит HUD: значения и причины для пустых мест.
//! Сверять с оверлеем NVIDIA, HWiNFO или Ryzen Master.
//!
//! ```powershell
//! cargo run -p mh-sources --example live_hardware --release -- --seconds 20
//! ```
//!
//! Температура и мощность CPU требуют драйвера PawnIO и прав администратора (PLAN.md §2.14).

#[cfg(not(windows))]
fn main() {
    eprintln!("живая проверка датчиков требует Windows");
}

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use mh_core::{Aggregator, SampleTier, SectionHealth, SensorStatus};
    use mh_sources::hardware::cpuid::identify;
    use mh_sources::hardware::sampler::HardwareSampler;

    const GIB: f64 = (1u64 << 30) as f64;

    fn value<T>(value: Option<T>, format: impl Fn(T) -> String) -> String {
        value.map(format).unwrap_or_else(|| "—".into())
    }

    fn health(health: SectionHealth) -> String {
        match (health.status, health.reason) {
            (SensorStatus::Available, _) => String::new(),
            (status, Some(reason)) => format!("  [{status:?}: {reason}]"),
            (status, None) => format!("  [{status:?}]"),
        }
    }

    let seconds: u64 = std::env::args()
        .skip_while(|arg| arg != "--seconds")
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(15);

    let cpu = identify();
    println!(
        "CPU: {} | {:?}, семейство {:#x}, модель {:#x}",
        cpu.brand, cpu.vendor, cpu.family, cpu.model
    );

    let mut sampler = HardwareSampler::open();
    let mut aggregator = Aggregator::new();
    let started = Instant::now();
    let mut tick = 0u64;

    while started.elapsed() < Duration::from_secs(seconds) {
        let now_ms = started.elapsed().as_millis() as u64;
        aggregator.submit_hardware(
            SampleTier::Load,
            sampler.sample(SampleTier::Load, now_ms),
            now_ms,
        );
        if tick.is_multiple_of(2) {
            aggregator.submit_hardware(
                SampleTier::Slow,
                sampler.sample(SampleTier::Slow, now_ms),
                now_ms,
            );
            let s = aggregator.snapshot(now_ms);
            println!("[{:>3} с] железо: {:?}", now_ms / 1_000, s.hardware_status);
            println!(
                "  GPU {}: {} | {} | ядро {} | память {} | VRAM {} / {} | {} | вент. {}{}",
                s.gpu.name.as_deref().unwrap_or("?"),
                value(s.gpu.load_percent, |v| format!("{v:.0} %")),
                value(s.gpu.temperature_c, |v| format!("{v:.0} °C")),
                value(s.gpu.core_clock_mhz, |v| format!("{v:.0} МГц")),
                value(s.gpu.memory_clock_mhz, |v| format!("{v:.0} МГц")),
                value(s.gpu.vram_used_bytes, |v| format!("{:.2}", v as f64 / GIB)),
                value(s.gpu.vram_total_bytes, |v| format!("{:.2} ГиБ", v as f64 / GIB)),
                value(s.gpu.power_watts, |v| format!("{v:.1} Вт")),
                value(s.gpu.fan_rpm, |v| format!("{v:.0} об/мин")),
                health(s.gpu.health),
            );
            println!(
                "  CPU: {} | {} | {} | {}{}",
                value(s.cpu.load_percent, |v| format!("{v:.0} %")),
                value(s.cpu.temperature_c, |v| format!("{v:.1} °C")),
                value(s.cpu.clock_mhz, |v| format!("{v:.0} МГц")),
                value(s.cpu.power_watts, |v| format!("{v:.1} Вт")),
                health(s.cpu.health),
            );
            println!(
                "  RAM: {} / {} ({}){}",
                value(s.memory.used_bytes, |v| format!("{:.1}", v as f64 / GIB)),
                value(s.memory.total_bytes, |v| format!("{:.1} ГиБ", v as f64 / GIB)),
                value(s.memory.load_percent(), |v| format!("{v:.0} %")),
                health(s.memory.health),
            );
        }
        tick += 1;
        std::thread::sleep(Duration::from_millis(500));
    }
}

//! Сравнение путей чтения видеокарты: NVML и WDDM (D3DKMT + PDH).
//!
//! На машине с NVIDIA показывает оба столбца рядом — так проверяется, что путь для AMD и Intel
//! даёт правдоподобные цифры. Без NVIDIA показывает только WDDM. В конце — процессы с
//! наибольшей загрузкой 3D: по ней выбор цели решает, рисует ли кандидат.
//!
//! ```powershell
//! cargo run -p mh-sources --example probe_gpu -- 10
//! ```

#[cfg(windows)]
fn main() {
    use mh_sources::hardware::nvml::NvmlSensor;
    use mh_sources::hardware::wddm::WddmGpuSensor;

    let seconds: u32 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    for info in mh_platform::gpu::adapters() {
        println!(
            "адаптер: {} ({}), память {} МБ{}",
            info.name,
            info.luid.pdh_tag(),
            info.dedicated_memory_bytes / (1 << 20),
            if info.software { ", программный" } else { "" }
        );
    }
    let nvml = NvmlSensor::open().ok();
    let mut wddm = match WddmGpuSensor::open() {
        Ok(sensor) => sensor,
        Err(reason) => {
            println!("WDDM: {}", reason.message());
            return;
        }
    };
    println!("WDDM выбрал: {}", wddm.name());
    let _ = wddm.read_load();
    let show = |value: Option<f64>| value.map_or("—".to_string(), |v| format!("{v:.0}"));
    for _ in 0..seconds {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let (w_load, w_slow) = (wddm.read_load(), wddm.read_slow());
        let (n_load, n_slow) = match &nvml {
            Some(sensor) => (sensor.read_load(), sensor.read_slow()),
            None => Default::default(),
        };
        let mb = |bytes: Option<u64>| bytes.map(|b| (b / (1 << 20)) as f64);
        println!(
            "загрузка {:>4} | {:<4}  темп. {:>3} | {:<3}  ядро МГц {:>5} | {:<5}  память МГц {:>5} | {:<5}  VRAM МБ {:>5} | {:<5}  вент. {:>5} | {:<5}   (NVML | WDDM)",
            show(n_load.load_percent),
            show(w_load.load_percent),
            show(n_slow.temperature_c),
            show(w_slow.temperature_c),
            show(n_load.core_clock_mhz),
            show(w_load.core_clock_mhz),
            show(n_load.memory_clock_mhz),
            show(w_load.memory_clock_mhz),
            show(mb(n_slow.vram_used_bytes)),
            show(mb(w_slow.vram_used_bytes)),
            show(n_slow.fan_rpm),
            show(w_slow.fan_rpm),
        );
    }

    use mh_core::ProcessLookup;
    let mut query =
        mh_platform::pdh::CounterQuery::open(&[r"\GPU Engine(*)\Utilization Percentage"]).unwrap();
    query.collect().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    query.collect().unwrap();
    let instances = query.instances(0);
    let mut pids: Vec<u32> = instances
        .iter()
        .filter_map(|(name, _)| name.strip_prefix("pid_")?.split('_').next()?.parse().ok())
        .collect();
    pids.sort_unstable();
    pids.dedup();
    let lookup = mh_platform::process::SystemProcessLookup;
    let mut loads: Vec<(f64, u32)> = pids
        .into_iter()
        .map(|pid| (mh_sources::hardware::gpu_counters::process_3d_load(&instances, pid), pid))
        .collect();
    loads.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("загрузка 3D по процессам (порог «рисует» — 10 %):");
    for (load, pid) in loads.into_iter().take(8) {
        let name = lookup.by_pid(pid).map(|p| p.executable).unwrap_or_default();
        println!("  {load:>5.1} %  {pid:>6}  {name}");
    }
}

#[cfg(not(windows))]
fn main() {}

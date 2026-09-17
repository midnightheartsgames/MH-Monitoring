//! Разбор счётчиков PDH видеокарт: `GPU Engine` и `GPU Adapter Memory`.
//!
//! Имена экземпляров устроены так:
//!
//! ```text
//! GPU Engine:          pid_1234_luid_0x00000000_0x0000D1F2_phys_0_eng_0_engtype_3D
//! GPU Adapter Memory:  luid_0x00000000_0x0000D1F2_phys_0
//! ```
//!
//! Чистые функции, без Windows: так их проверяют тесты на любой машине.

/// Загрузка адаптера так, как её считает диспетчер задач: сумма по процессам для каждого движка,
/// затем максимум по движкам. `tag` — LUID адаптера в виде `luid_0x…_0x…`.
pub fn adapter_load(instances: &[(String, f64)], tag: &str) -> Option<f64> {
    let tag = tag.to_ascii_lowercase();
    let mut engines: Vec<(u32, f64)> = Vec::new();
    for (name, value) in instances {
        let name = name.to_ascii_lowercase();
        if !name.contains(&tag) {
            continue;
        }
        let Some(engine) = number_after(&name, "_eng_") else { continue };
        match engines.iter_mut().find(|(id, _)| *id == engine) {
            Some((_, total)) => *total += value,
            None => engines.push((engine, *value)),
        }
    }
    engines.into_iter().map(|(_, total)| total).reduce(f64::max).map(|load| load.clamp(0.0, 100.0))
}

/// Загрузка 3D-движков процессом `pid` на самом нагруженном адаптере. Отвечает на вопрос
/// «рисует ли этот процесс».
pub fn process_3d_load(instances: &[(String, f64)], pid: u32) -> f64 {
    let prefix = format!("pid_{pid}_");
    let mut adapters: Vec<(&str, f64)> = Vec::new();
    for (name, value) in instances {
        if !name.starts_with(&prefix) || !name.to_ascii_lowercase().ends_with("_engtype_3d") {
            continue;
        }
        // Адаптер — всё между `pid_N_` и `_eng_`.
        let rest = &name[prefix.len()..];
        let adapter = rest.find("_eng_").map_or(rest, |end| &rest[..end]);
        match adapters.iter_mut().find(|(id, _)| *id == adapter) {
            Some((_, total)) => *total += value,
            None => adapters.push((adapter, *value)),
        }
    }
    adapters.into_iter().map(|(_, total)| total).fold(0.0, f64::max).clamp(0.0, 100.0)
}

/// Занятая выделенная видеопамять адаптера, в байтах.
pub fn adapter_memory(instances: &[(String, f64)], tag: &str) -> Option<u64> {
    let tag = tag.to_ascii_lowercase();
    instances
        .iter()
        .filter(|(name, _)| name.to_ascii_lowercase().starts_with(&tag))
        .map(|(_, value)| value.max(0.0) as u64)
        .reduce(|a, b| a + b)
}

fn number_after(name: &str, marker: &str) -> Option<u32> {
    let start = name.find(marker)? + marker.len();
    let digits: String = name[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "luid_0x00000000_0x0000d1f2";

    fn engine(pid: u32, luid_low: &str, eng: u32, kind: &str, value: f64) -> (String, f64) {
        (
            format!("pid_{pid}_luid_0x00000000_0x0000{luid_low}_phys_0_eng_{eng}_engtype_{kind}"),
            value,
        )
    }

    #[test]
    fn adapter_load_sums_processes_per_engine_and_takes_the_busiest_engine() {
        let instances = vec![
            engine(10, "D1F2", 0, "3D", 40.0),
            engine(11, "D1F2", 0, "3D", 25.0),
            engine(10, "D1F2", 4, "VideoDecode", 30.0),
            // Другой адаптер не считается.
            engine(12, "AAAA", 0, "3D", 90.0),
        ];
        assert_eq!(adapter_load(&instances, TAG), Some(65.0));
    }

    #[test]
    fn adapter_load_is_capped_and_absent_without_instances() {
        let instances = vec![engine(10, "D1F2", 0, "3D", 70.0), engine(11, "D1F2", 0, "3D", 70.0)];
        assert_eq!(adapter_load(&instances, TAG), Some(100.0));
        assert_eq!(adapter_load(&[], TAG), None);
    }

    #[test]
    fn an_idle_adapter_reads_zero_not_nothing() {
        let instances = vec![engine(10, "D1F2", 0, "3D", 0.0)];
        assert_eq!(adapter_load(&instances, TAG), Some(0.0));
    }

    #[test]
    fn process_load_counts_only_its_3d_engines() {
        let instances = vec![
            engine(10, "D1F2", 0, "3D", 30.0),
            engine(10, "D1F2", 1, "3D", 5.0),
            engine(10, "D1F2", 4, "VideoDecode", 50.0),
            engine(100, "D1F2", 0, "3D", 80.0),
            engine(10, "AAAA", 0, "3D", 20.0),
        ];
        assert_eq!(process_3d_load(&instances, 10), 35.0);
        // `pid_1` не совпадает с `pid_10` и `pid_100`.
        assert_eq!(process_3d_load(&instances, 1), 0.0);
    }

    #[test]
    fn memory_is_read_for_the_adapter() {
        let instances = vec![
            ("luid_0x00000000_0x0000D1F2_phys_0".to_string(), 2_147_483_648.0),
            ("luid_0x00000000_0x0000AAAA_phys_0".to_string(), 1.0),
        ];
        assert_eq!(adapter_memory(&instances, TAG), Some(2_147_483_648));
        assert_eq!(adapter_memory(&instances, "luid_0x00000000_0x00000001"), None);
    }
}

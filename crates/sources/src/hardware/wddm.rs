//! Видеокарта любого производителя через WDDM — запасной путь, когда NVML нет.
//!
//! Так читаются AMD и Intel: имя, объём памяти, температура, вентилятор и частоты — из ядра
//! графики (D3DKMT), загрузка и занятая память — из счётчиков PDH. Это те же данные, что
//! показывает диспетчер задач. Мощности в ваттах здесь нет: ядро сообщает её только долей от
//! предела, поэтому строка питания остаётся пустой.

use mh_core::{GpuStats, SectionHealth, SensorReason};
use mh_platform::gpu::{Adapter, adapters};
use mh_platform::pdh::CounterQuery;

use super::gpu_counters::{adapter_load, adapter_memory};

const ENGINE_COUNTER: &str = r"\GPU Engine(*)\Utilization Percentage";
const MEMORY_COUNTER: &str = r"\GPU Adapter Memory(*)\Dedicated Usage";

pub struct WddmGpuSensor {
    adapter: Adapter,
    tag: String,
    name: String,
    total_bytes: u64,
    counters: Option<CounterQuery>,
}

impl WddmGpuSensor {
    /// Выбирает карту с наибольшей выделенной памятью: в игровой машине это и есть основная, а
    /// встроенная графика рядом с ней памяти почти не имеет.
    pub fn open() -> Result<Self, SensorReason> {
        let info = adapters()
            .into_iter()
            .filter(|info| !info.software && info.dedicated_memory_bytes > 0)
            .max_by_key(|info| info.dedicated_memory_bytes)
            .ok_or(SensorReason::NoSupportedGpu)?;
        let adapter = Adapter::open(info.luid).ok_or(SensorReason::GpuQueryFailed)?;
        // Без счётчиков остаются датчики ядра — это лучше, чем ничего.
        let mut counters = CounterQuery::open(&[ENGINE_COUNTER, MEMORY_COUNTER]).ok();
        if let Some(query) = counters.as_mut() {
            let _ = query.collect();
        }
        Ok(Self {
            adapter,
            tag: info.luid.pdh_tag(),
            name: info.name,
            total_bytes: info.dedicated_memory_bytes,
            counters,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Частый тир: загрузка и частоты. Снимает счётчики — редкий тир читает их же.
    pub fn read_load(&mut self) -> GpuStats {
        let load_percent = self.counters.as_mut().and_then(|query| {
            query.collect().ok()?;
            adapter_load(&query.instances(0), &self.tag)
        });
        let perf = self.adapter.perf();
        GpuStats {
            health: self.health(load_percent.is_some()),
            load_percent,
            core_clock_mhz: perf.core_frequency_mhz,
            memory_clock_mhz: perf.memory_frequency_mhz,
            ..Default::default()
        }
    }

    /// Редкий тир: имя, температура, видеопамять, вентилятор.
    pub fn read_slow(&mut self) -> GpuStats {
        let used =
            self.counters.as_ref().and_then(|query| adapter_memory(&query.instances(1), &self.tag));
        let perf = self.adapter.perf();
        GpuStats {
            health: self.health(used.is_some()),
            name: Some(self.name.clone()),
            temperature_c: perf.temperature_c,
            fan_rpm: perf.fan_rpm.map(f64::from),
            vram_used_bytes: used,
            vram_total_bytes: Some(self.total_bytes),
            ..Default::default()
        }
    }

    /// Счётчики не открылись или молчат — секция частичная, но не сломанная.
    fn health(&self, counters_read: bool) -> SectionHealth {
        if counters_read {
            SectionHealth::available()
        } else {
            SectionHealth::partial(SensorReason::GpuQueryFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Любая машина с видеокартой открывает её этим путём; на машине разработчика — NVIDIA,
    /// и загрузка с памятью обязаны читаться.
    #[test]
    fn this_machine_either_reads_or_explains() {
        match WddmGpuSensor::open() {
            Ok(mut sensor) => {
                assert!(!sensor.name().is_empty());
                let _ = sensor.read_load();
                std::thread::sleep(std::time::Duration::from_millis(300));
                let load = sensor.read_load();
                let slow = sensor.read_slow();
                assert!(load.load_percent.is_some(), "{load:?}");
                assert!(slow.vram_total_bytes.is_some_and(|total| total > 0));
                assert!(slow.vram_used_bytes.is_some(), "{slow:?}");
            }
            Err(reason) => assert!(!reason.message().is_empty()),
        }
    }
}

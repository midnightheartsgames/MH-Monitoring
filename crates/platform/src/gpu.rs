//! Видеокарты через ядро графики Windows (D3DKMT) — для любого производителя.
//!
//! Отсюда же берёт данные диспетчер задач: имя, объём видеопамяти, температура, обороты
//! вентилятора и частоты. Прав администратора не нужно, работает и из службы. Драйвер сообщает
//! только то, что поддерживает (WDDM 2.4+), — чего нет, то `None`.
//!
//! Загрузка и занятая видеопамять здесь не читаются: их отдают счётчики PDH `GPU Engine` и
//! `GPU Adapter Memory`, экземпляры которых помечены LUID адаптера — см. [`Luid::pdh_tag`].

use windows_sys::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTER_PERFDATA, D3DKMT_ADAPTER_PERFDATACAPS, D3DKMT_ADAPTERINFO,
    D3DKMT_ADAPTERREGISTRYINFO, D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS2, D3DKMT_NODE_PERFDATA,
    D3DKMT_OPENADAPTERFROMLUID, D3DKMT_QUERYADAPTERINFO, D3DKMT_SEGMENTSIZEINFO,
    D3DKMTCloseAdapter, D3DKMTEnumAdapters2, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo,
    KMTQAITYPE_ADAPTERPERFDATA, KMTQAITYPE_ADAPTERPERFDATA_CAPS, KMTQAITYPE_ADAPTERREGISTRYINFO,
    KMTQAITYPE_ADAPTERTYPE, KMTQAITYPE_GETSEGMENTSIZE, KMTQAITYPE_NODEPERFDATA,
    KMTQUERYADAPTERINFOTYPE,
};
use windows_sys::Win32::Foundation::LUID;

use crate::sys::from_wide;

/// Бит `SoftwareDevice` в `D3DKMT_ADAPTERTYPE`: Microsoft Basic Render Driver и подобные.
const ADAPTER_TYPE_SOFTWARE: u32 = 1 << 2;

/// Идентификатор адаптера, стабильный до перезагрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Luid {
    pub high: i32,
    pub low: u32,
}

impl Luid {
    /// Как LUID пишется в именах экземпляров PDH: `luid_0x00000000_0x0000D1F2`.
    pub fn pdh_tag(self) -> String {
        format!("luid_0x{:08x}_0x{:08x}", self.high as u32, self.low)
    }

    fn raw(self) -> LUID {
        LUID { HighPart: self.high, LowPart: self.low }
    }
}

/// Что известно об адаптере без опроса датчиков.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    pub luid: Luid,
    pub name: String,
    pub dedicated_memory_bytes: u64,
    /// Программный рендер — не видеокарта.
    pub software: bool,
}

/// Все адаптеры, которые видит ядро графики.
pub fn adapters() -> Vec<AdapterInfo> {
    let mut request = D3DKMT_ENUMADAPTERS2 { NumAdapters: 0, pAdapters: std::ptr::null_mut() };
    if unsafe { D3DKMTEnumAdapters2(&mut request) } < 0 || request.NumAdapters == 0 {
        return Vec::new();
    }
    let mut list = vec![D3DKMT_ADAPTERINFO::default(); request.NumAdapters as usize];
    request.pAdapters = list.as_mut_ptr();
    if unsafe { D3DKMTEnumAdapters2(&mut request) } < 0 {
        return Vec::new();
    }
    list.truncate(request.NumAdapters as usize);
    // Перечисление открывает каждый адаптер — закрыть все, даже если о каком-то ничего не узнали.
    let found = list
        .iter()
        .map(|entry| {
            let luid = Luid { high: entry.AdapterLuid.HighPart, low: entry.AdapterLuid.LowPart };
            let registry: Option<D3DKMT_ADAPTERREGISTRYINFO> =
                query(entry.hAdapter, KMTQAITYPE_ADAPTERREGISTRYINFO, zeroed());
            let segments: Option<D3DKMT_SEGMENTSIZEINFO> =
                query(entry.hAdapter, KMTQAITYPE_GETSEGMENTSIZE, Default::default());
            let kind: Option<u32> = query(entry.hAdapter, KMTQAITYPE_ADAPTERTYPE, 0);
            AdapterInfo {
                luid,
                name: registry.map(|info| from_wide(&info.AdapterString)).unwrap_or_default(),
                dedicated_memory_bytes: segments.map_or(0, |s| s.DedicatedVideoMemorySize),
                software: kind.is_some_and(|bits| bits & ADAPTER_TYPE_SOFTWARE != 0),
            }
        })
        .collect();
    for entry in &list {
        let close = D3DKMT_CLOSEADAPTER { hAdapter: entry.hAdapter };
        unsafe { D3DKMTCloseAdapter(&close) };
    }
    found
}

/// Показания датчиков адаптера. `None` — драйвер это поле не сообщает.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AdapterPerf {
    pub temperature_c: Option<f64>,
    pub fan_rpm: Option<u32>,
    pub memory_frequency_mhz: Option<f64>,
    /// Частота графического ядра — узла 0, на котором у всех драйверов работает 3D.
    pub core_frequency_mhz: Option<f64>,
}

/// Открытый адаптер для периодического опроса.
pub struct Adapter {
    handle: u32,
    has_temperature: bool,
    has_fan: bool,
}

impl Adapter {
    pub fn open(luid: Luid) -> Option<Adapter> {
        let mut request = D3DKMT_OPENADAPTERFROMLUID { AdapterLuid: luid.raw(), hAdapter: 0 };
        if unsafe { D3DKMTOpenAdapterFromLuid(&mut request) } < 0 {
            return None;
        }
        // Нули в показаниях неоднозначны: 0 об/мин — это и «вентилятор стоит», и «нет датчика».
        // Возможности адаптера это различают.
        let caps: Option<D3DKMT_ADAPTER_PERFDATACAPS> =
            query(request.hAdapter, KMTQAITYPE_ADAPTERPERFDATA_CAPS, Default::default());
        Some(Adapter {
            handle: request.hAdapter,
            has_temperature: caps.is_some_and(|caps| caps.TemperatureMax > 0),
            has_fan: caps.is_some_and(|caps| caps.MaxFanRPM > 0),
        })
    }

    pub fn perf(&self) -> AdapterPerf {
        let adapter: Option<D3DKMT_ADAPTER_PERFDATA> =
            query(self.handle, KMTQAITYPE_ADAPTERPERFDATA, Default::default());
        let node: Option<D3DKMT_NODE_PERFDATA> =
            query(self.handle, KMTQAITYPE_NODEPERFDATA, Default::default());
        let hz_to_mhz = |hz: u64| (hz > 0).then(|| hz as f64 / 1_000_000.0);
        AdapterPerf {
            // Десятые доли градуса.
            temperature_c: adapter
                .filter(|_| self.has_temperature)
                .map(|data| f64::from(data.Temperature) / 10.0),
            fan_rpm: adapter.filter(|_| self.has_fan).map(|data| data.FanRPM),
            memory_frequency_mhz: adapter.and_then(|data| hz_to_mhz(data.MemoryFrequency)),
            core_frequency_mhz: node.and_then(|data| hz_to_mhz(data.Frequency)),
        }
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        let close = D3DKMT_CLOSEADAPTER { hAdapter: self.handle };
        unsafe { D3DKMTCloseAdapter(&close) };
    }
}

// Дескриптор адаптера — число ядра, к потоку не привязан.
unsafe impl Send for Adapter {}

/// `D3DKMTQueryAdapterInfo` с буфером типа `T`. Входные поля (индекс адаптера, узла) — нули.
fn query<T: Copy>(handle: u32, kind: KMTQUERYADAPTERINFOTYPE, mut buffer: T) -> Option<T> {
    let mut request = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: kind,
        pPrivateDriverData: (&mut buffer as *mut T).cast(),
        PrivateDriverDataSize: std::mem::size_of::<T>() as u32,
    };
    (unsafe { D3DKMTQueryAdapterInfo(&mut request) } >= 0).then_some(buffer)
}

fn zeroed<T: Copy>() -> T {
    // SAFETY: только для C-структур из чисел и массивов чисел.
    unsafe { std::mem::zeroed() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_luid_is_written_the_way_pdh_writes_it() {
        assert_eq!(Luid { high: 0, low: 0xD1F2 }.pdh_tag(), "luid_0x00000000_0x0000d1f2");
        assert_eq!(Luid { high: -1, low: 1 }.pdh_tag(), "luid_0xffffffff_0x00000001");
    }

    /// На любой Windows есть хотя бы программный адаптер; на машине разработчика — и настоящая
    /// карта с именем и памятью. У виртуального адаптера машины CI (GitHub Actions) нет ни
    /// имени, ни памяти, и программным он себя не называет: имя проверяется только у карты с
    /// памятью.
    #[test]
    fn adapters_are_listed_and_can_be_opened() {
        let list = adapters();
        assert!(!list.is_empty());
        for info in list.iter().filter(|info| !info.software) {
            if info.dedicated_memory_bytes > 0 {
                assert!(!info.name.is_empty(), "{info:?}");
            }
            let adapter = Adapter::open(info.luid).expect("адаптер из списка открывается");
            let _ = adapter.perf();
        }
    }
}

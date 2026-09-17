//! Счётчики производительности Windows (PDH).
//!
//! Пути добавляются английскими именами (`PdhAddEnglishCounterW`): локализованные имена на
//! русской Windows другие, а английские работают везде.

use windows_sys::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW,
};

use crate::sys::wide;

/// Не обрезать проценты до 100. В `windows-sys` константы нет; значение из `pdh.h`.
/// Нужен, например, для `% Processor Performance`, который при бусте выше 100.
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;

/// Код ошибки PDH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdhError(pub u32);

impl std::fmt::Display for PdhError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "PDH: {:#010x}", self.0)
    }
}

impl std::error::Error for PdhError {}

/// Запрос из нескольких счётчиков, собираемых разом.
pub struct CounterQuery {
    query: PDH_HQUERY,
    counters: Vec<PDH_HCOUNTER>,
}

// Дескрипторы PDH не привязаны к потоку; одновременный доступ исключает `&mut self` в `collect`.
unsafe impl Send for CounterQuery {}

impl CounterQuery {
    /// Открывает запрос. Счётчик, который на этой машине не существует, — ошибка целиком:
    /// вызывающий решает, чем его заменить.
    pub fn open(paths: &[&str]) -> Result<Self, PdhError> {
        let mut query: PDH_HQUERY = std::ptr::null_mut();
        check(unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) })?;
        let mut this = Self { query, counters: Vec::with_capacity(paths.len()) };
        for path in paths {
            let path = wide(path);
            let mut counter: PDH_HCOUNTER = std::ptr::null_mut();
            check(unsafe { PdhAddEnglishCounterW(this.query, path.as_ptr(), 0, &mut counter) })?;
            this.counters.push(counter);
        }
        Ok(this)
    }

    /// Снимает замер. Счётчикам-скоростям нужны два замера, прежде чем появится значение.
    pub fn collect(&mut self) -> Result<(), PdhError> {
        check(unsafe { PdhCollectQueryData(self.query) })
    }

    /// Значение счётчика с индексом `index` из последнего замера, если оно есть.
    pub fn value(&self, index: usize) -> Option<f64> {
        let counter = *self.counters.get(index)?;
        let mut value = PDH_FMT_COUNTERVALUE::default();
        let status = unsafe {
            PdhGetFormattedCounterValue(
                counter,
                PDH_FMT_DOUBLE | PDH_FMT_NOCAP100,
                std::ptr::null_mut(),
                &mut value,
            )
        };
        if status != 0 || !matches!(value.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA) {
            return None;
        }
        let number = unsafe { value.Anonymous.doubleValue };
        number.is_finite().then_some(number)
    }

    /// Все экземпляры счётчика с `*` в пути: имя экземпляра и значение.
    ///
    /// Экземпляры приходят и уходят вместе с процессами — PDH перечисляет их заново на каждом
    /// замере. Экземпляр без годного значения пропускается.
    pub fn instances(&self, index: usize) -> Vec<(String, f64)> {
        let Some(&counter) = self.counters.get(index) else { return Vec::new() };
        let format = PDH_FMT_DOUBLE | PDH_FMT_NOCAP100;
        let mut size = 0u32;
        let mut count = 0u32;
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                format,
                &mut size,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if status != PDH_MORE_DATA || size == 0 {
            return Vec::new();
        }
        // Буфер — в байтах: элементы, а за ними строки имён.
        let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let mut buffer =
            vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); (size as usize).div_ceil(item)];
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                format,
                &mut size,
                &mut count,
                buffer.as_mut_ptr(),
            )
        };
        if status != 0 {
            return Vec::new();
        }
        buffer[..count as usize]
            .iter()
            .filter(|entry| {
                matches!(entry.FmtValue.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA)
            })
            .filter_map(|entry| {
                let value = unsafe { entry.FmtValue.Anonymous.doubleValue };
                let name = unsafe { crate::sys::from_wide_ptr(entry.szName) };
                value.is_finite().then_some((name, value))
            })
            .collect()
    }
}

impl Drop for CounterQuery {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

fn check(status: u32) -> Result<(), PdhError> {
    if status == 0 { Ok(()) } else { Err(PdhError(status)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_counter_is_an_error() {
        assert!(CounterQuery::open(&[r"\No Such Object(_Total)\No Such Counter"]).is_err());
    }

    #[test]
    fn processor_performance_is_readable_and_uncapped_type() {
        let mut query = CounterQuery::open(&[
            r"\Processor Information(_Total)\% Processor Performance",
            r"\Processor Information(_Total)\Processor Frequency",
        ])
        .expect("счётчики процессора есть на любой Windows 10+");
        query.collect().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        query.collect().unwrap();
        let performance = query.value(0).expect("со второго замера значение есть");
        let frequency = query.value(1).expect("номинальная частота");
        assert!(performance > 0.0 && performance < 1_000.0, "{performance}");
        assert!(frequency > 100.0, "{frequency}");
        assert_eq!(query.value(5), None);
    }

    #[test]
    fn wildcard_instances_are_listed() {
        let mut query = CounterQuery::open(&[r"\Processor Information(*)\% Processor Time"])
            .expect("счётчик процессора есть на любой Windows 10+");
        query.collect().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        query.collect().unwrap();
        let instances = query.instances(0);
        assert!(instances.iter().any(|(name, _)| name == "_Total"), "{instances:?}");
        assert!(instances.len() > 1);
        assert!(query.instances(3).is_empty());
    }
}

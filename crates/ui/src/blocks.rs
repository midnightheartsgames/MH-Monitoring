//! Настройки блоков HUD: видеокарта, процессор, ОЗУ, FPS (схема v3).
//!
//! У всех блоков одна структура. Поля, которые блоку не нужны (пороги у ОЗУ, имя устройства у
//! FPS), он просто не читает: так файл остаётся однообразным, а код HUD — без особых случаев.
//!
//! Цвета хранятся как «переопределение»: `None` значит цвет темы. Тогда «по умолчанию» — это
//! отсутствие значения, а не копия палитры, которая устареет при первой же её правке.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Самое длинное своё название блока, в символах.
pub const MAX_TITLE_CHARS: usize = 32;
/// Пределы порогов температуры, °C.
pub const MIN_TEMPERATURE: f32 = 40.0;
pub const MAX_TEMPERATURE: f32 = 110.0;
/// С какой загрузки заголовок желтеет и краснеет, если включена цветовая индикация.
pub const LOAD_WARN_PERCENT: f64 = 80.0;
pub const LOAD_CRITICAL_PERCENT: f64 = 95.0;

/// Цвет sRGB; в файле — `"#RRGGBB"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    /// `#RRGGBB` или `RRGGBB`, регистр любой.
    pub fn parse(text: &str) -> Option<Rgb> {
        let hex = text.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
        Some(Rgb([channel(0)?, channel(2)?, channel(4)?]))
    }

    pub fn hex(self) -> String {
        let [r, g, b] = self.0;
        format!("#{r:02X}{g:02X}{b:02X}")
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

/// Негодный цвет — цвет темы, а не карантин всего файла.
fn lenient_color<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Rgb>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value.as_str().and_then(Rgb::parse))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BlockColors {
    #[serde(deserialize_with = "lenient_color", skip_serializing_if = "Option::is_none")]
    pub header: Option<Rgb>,
    #[serde(deserialize_with = "lenient_color", skip_serializing_if = "Option::is_none")]
    pub labels: Option<Rgb>,
    #[serde(deserialize_with = "lenient_color", skip_serializing_if = "Option::is_none")]
    pub values: Option<Rgb>,
    /// Крупная цифра и график FPS.
    #[serde(deserialize_with = "lenient_color", skip_serializing_if = "Option::is_none")]
    pub accent: Option<Rgb>,
}

impl BlockColors {
    pub fn is_default(&self) -> bool {
        *self == BlockColors::default()
    }
}

/// Когда значения меняют цвет.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// С этой температуры значение желтеет.
    pub warn_c: f32,
    /// С этой — краснеет.
    pub critical_c: f32,
    /// Красить и загрузку в заголовке: [`LOAD_WARN_PERCENT`], [`LOAD_CRITICAL_PERCENT`].
    pub color_load: bool,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { warn_c: 80.0, critical_c: 90.0, color_load: false }
    }
}

impl Thresholds {
    /// Пороги в пределах и по порядку: критический не ниже предупреждения.
    fn sanitized(self) -> Thresholds {
        let defaults = Thresholds::default();
        let clamp = |value: f32, fallback: f32| {
            if value.is_finite() {
                value.round().clamp(MIN_TEMPERATURE, MAX_TEMPERATURE)
            } else {
                fallback
            }
        };
        let warn_c = clamp(self.warn_c, defaults.warn_c);
        let critical_c = clamp(self.critical_c, defaults.critical_c).max(warn_c);
        Thresholds { warn_c, critical_c, color_load: self.color_load }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Block {
    pub enabled: bool,
    /// Своё название. Пустое — стандартное или имя устройства.
    pub title: String,
    /// Вместо «GPU»/«CPU» — модель, как её называет система.
    pub device_name: bool,
    pub colors: BlockColors,
    pub thresholds: Thresholds,
}

impl Default for Block {
    fn default() -> Self {
        Self {
            enabled: true,
            title: String::new(),
            device_name: false,
            colors: BlockColors::default(),
            thresholds: Thresholds::default(),
        }
    }
}

impl Block {
    /// Заголовок блока: своё название, имя устройства или `standard`.
    pub fn heading(&self, standard: &str, device: Option<&str>) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        match device.filter(|_| self.device_name) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => standard.to_string(),
        }
    }

    fn sanitized(mut self) -> Block {
        self.title = self.title.trim().chars().take(MAX_TITLE_CHARS).collect();
        self.thresholds = self.thresholds.sanitized();
        self
    }
}

/// Какой блок.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Gpu,
    Cpu,
    Ram,
    Fps,
}

impl BlockKind {
    pub const ALL: [BlockKind; 4] =
        [BlockKind::Gpu, BlockKind::Cpu, BlockKind::Ram, BlockKind::Fps];

    pub fn title(self) -> &'static str {
        match self {
            BlockKind::Gpu => "Видеокарта",
            BlockKind::Cpu => "Процессор",
            BlockKind::Ram => "ОЗУ",
            BlockKind::Fps => "FPS",
        }
    }
}

/// Порядок из файла: незнакомые имена пропускаются, а не портят весь файл.
fn lenient_order<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<BlockKind>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    let items = value.as_array().cloned().unwrap_or_default();
    Ok(items.into_iter().filter_map(|item| serde_json::from_value(item).ok()).collect())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Blocks {
    pub gpu: Block,
    pub cpu: Block,
    pub ram: Block,
    pub fps: Block,
    /// Сверху вниз. После починки — каждый блок ровно один раз.
    #[serde(deserialize_with = "lenient_order")]
    pub order: Vec<BlockKind>,
}

impl Default for Blocks {
    fn default() -> Self {
        Self {
            gpu: Block::default(),
            cpu: Block::default(),
            ram: Block::default(),
            fps: Block::default(),
            order: BlockKind::ALL.to_vec(),
        }
    }
}

impl Blocks {
    pub const KEYS: [&'static str; 4] = ["gpu", "cpu", "ram", "fps"];

    pub fn get(&self, kind: BlockKind) -> &Block {
        match kind {
            BlockKind::Gpu => &self.gpu,
            BlockKind::Cpu => &self.cpu,
            BlockKind::Ram => &self.ram,
            BlockKind::Fps => &self.fps,
        }
    }

    /// Сдвигает блок на `step` позиций (−1 — выше, +1 — ниже), если есть куда.
    pub fn shift(&mut self, index: usize, step: isize) {
        let Some(target) = index.checked_add_signed(step) else { return };
        if index < self.order.len() && target < self.order.len() {
            self.order.swap(index, target);
        }
    }

    pub fn sanitized(self) -> Blocks {
        // Повторы убираются, пропавшие блоки дописываются в конец в обычном порядке.
        let mut order: Vec<BlockKind> = Vec::with_capacity(BlockKind::ALL.len());
        for kind in self.order.into_iter().chain(BlockKind::ALL) {
            if !order.contains(&kind) {
                order.push(kind);
            }
        }
        Blocks {
            gpu: self.gpu.sanitized(),
            cpu: self.cpu.sanitized(),
            ram: self.ram.sanitized(),
            fps: self.fps.sanitized(),
            order,
        }
    }
}

/// Цвет по загрузке или температуре: `None`, пока значение ниже порога предупреждения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Warn,
    Critical,
}

pub fn level(value: Option<f64>, warn: f64, critical: f64) -> Option<Level> {
    match value? {
        v if v >= critical => Some(Level::Critical),
        v if v >= warn => Some(Level::Warn),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_with_or_without_a_hash() {
        assert_eq!(Rgb::parse("#3fd0d8"), Some(Rgb([0x3F, 0xD0, 0xD8])));
        assert_eq!(Rgb::parse(" 3FD0D8 "), Some(Rgb([0x3F, 0xD0, 0xD8])));
        assert_eq!(Rgb([1, 2, 255]).hex(), "#0102FF");
        for bad in ["", "#123", "#12345G", "#1234567", "#ЖЖЖ"] {
            assert_eq!(Rgb::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_bad_color_falls_back_to_the_theme_and_defaults_are_not_written() {
        let colors: BlockColors =
            serde_json::from_str(r##"{ "header": "#FF0000", "labels": "красный", "values": 5 }"##)
                .unwrap();
        assert_eq!(colors.header, Some(Rgb([255, 0, 0])));
        assert_eq!(colors.labels, None);
        assert_eq!(colors.values, None);
        assert_eq!(serde_json::to_string(&colors).unwrap(), r##"{"header":"#FF0000"}"##);
    }

    #[test]
    fn thresholds_are_clamped_and_ordered() {
        let fixed = Thresholds { warn_c: 95.4, critical_c: 70.0, color_load: true }.sanitized();
        assert_eq!((fixed.warn_c, fixed.critical_c), (95.0, 95.0));
        let fixed =
            Thresholds { warn_c: f32::NAN, critical_c: 500.0, color_load: false }.sanitized();
        assert_eq!((fixed.warn_c, fixed.critical_c), (80.0, MAX_TEMPERATURE));
    }

    #[test]
    fn the_heading_prefers_the_own_title_then_the_device() {
        let mut block = Block::default();
        assert_eq!(block.heading("GPU", Some("RTX 5070 Ti")), "GPU");
        block.device_name = true;
        assert_eq!(block.heading("GPU", Some("RTX 5070 Ti")), "RTX 5070 Ti");
        assert_eq!(block.heading("GPU", None), "GPU", "имени нет — стандартное");
        block.title = "Видяха".into();
        assert_eq!(block.heading("GPU", Some("RTX 5070 Ti")), "Видяха");
    }

    #[test]
    fn a_long_title_is_trimmed() {
        let block = Block { title: format!("  {}  ", "я".repeat(40)), ..Default::default() };
        assert_eq!(block.sanitized().title.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn the_order_is_repaired() {
        let blocks: Blocks =
            serde_json::from_str(r#"{ "order": ["fps", "vram", "fps", 3, "cpu"] }"#).unwrap();
        assert_eq!(blocks.order, [BlockKind::Fps, BlockKind::Fps, BlockKind::Cpu]);
        let order = blocks.sanitized().order;
        assert_eq!(order, [BlockKind::Fps, BlockKind::Cpu, BlockKind::Gpu, BlockKind::Ram]);
    }

    #[test]
    fn shifting_stays_inside_the_list() {
        let mut blocks = Blocks::default();
        blocks.shift(0, -1);
        blocks.shift(3, 1);
        assert_eq!(blocks.order, BlockKind::ALL);
        blocks.shift(3, -1);
        assert_eq!(blocks.order, [BlockKind::Gpu, BlockKind::Cpu, BlockKind::Fps, BlockKind::Ram]);
    }

    #[test]
    fn levels_start_at_their_thresholds() {
        assert_eq!(level(Some(79.9), 80.0, 90.0), None);
        assert_eq!(level(Some(80.0), 80.0, 90.0), Some(Level::Warn));
        assert_eq!(level(Some(90.0), 80.0, 90.0), Some(Level::Critical));
        assert_eq!(level(None, 80.0, 90.0), None);
    }
}

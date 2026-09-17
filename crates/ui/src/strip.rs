//! HUD в одну полосу — режим «Строка».
//!
//! У каждого блока — название, главное число и два-три включённых значения. Остальные строки
//! полного HUD сюда не попадают: полоса нужна, чтобы занимать минимум места над игрой.
//! Отсутствующее значение в полосе просто не рисуется — ряд прочерков её только удлинил бы.
//! Исключение — главное число блока: оно держит место.

use eframe::egui::{Color32, FontId, RichText, Sense, Stroke, Ui, vec2};
use mh_core::{FpsAvailability, Snapshot};

use crate::blocks::{self, Block, BlockKind, LOAD_CRITICAL_PERCENT, LOAD_WARN_PERCENT};
use crate::format::{self, EMPTY, WARMING_UP};
use crate::hud::Extras;
use crate::settings::{Metric, Settings};
use crate::theme::{self, Palette};

/// Размеры шрифтов относительно высоты полосы.
const BIG: f32 = 0.62;
const SMALL: f32 = 0.44;

struct Strip<'a> {
    settings: &'a Settings,
    warming_up: bool,
    big: f32,
    small: f32,
    /// Нарисован ли уже хоть один сегмент — перед следующим нужен разделитель.
    started: bool,
}

impl Strip<'_> {
    fn enabled(&self, metric: Metric) -> bool {
        self.settings.metrics.is_enabled(metric)
    }

    fn separate(&mut self, ui: &mut Ui) {
        if self.started {
            // Черта рисуется сама: символа «│» в Cuprum нет.
            let size = vec2(self.small * 1.8, self.big);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let stroke = Stroke::new(1.0, theme::TEXT_DISABLED);
            ui.painter().vline(rect.center().x, rect.y_range().shrink(self.big * 0.1), stroke);
        }
        self.started = true;
    }

    fn title(&self, ui: &mut Ui, title: &str, color: Color32) {
        text(ui, title, theme::bold(self.small), color);
        ui.add_space(self.small * 0.45);
    }

    fn main(&self, ui: &mut Ui, value: Option<String>, color: Color32) {
        let fallback = if self.warming_up { WARMING_UP } else { EMPTY };
        let value = value.unwrap_or_else(|| fallback.to_string());
        text(ui, &value, theme::bold(self.big), color);
    }

    /// Дополнительное значение с короткой подписью; нет значения — нет и подписи.
    fn extra(
        &self,
        ui: &mut Ui,
        label: &str,
        value: Option<String>,
        palette: Palette,
        color: Color32,
    ) {
        let Some(value) = value else { return };
        ui.add_space(self.small * 0.7);
        if !label.is_empty() {
            text(ui, label, theme::regular(self.small * 0.9), palette.labels);
            ui.add_space(self.small * 0.25);
        }
        text(ui, &value, theme::regular(self.small), color);
    }
}

fn text(ui: &mut Ui, value: &str, font: FontId, color: Color32) {
    ui.label(RichText::new(value).font(font).color(color));
}

/// Рисует полосу. Высота берётся из настроек.
pub fn contents(ui: &mut Ui, snapshot: &Snapshot, settings: &Settings, extras: &Extras<'_>) {
    let height = settings.overlay.strip_height;
    let mut strip = Strip {
        settings,
        warming_up: snapshot.is_warming_up(),
        big: height * BIG,
        small: height * SMALL,
        started: false,
    };
    // Горизонтальная раскладка не переносит: полоса шире окна, и окно подстраивается под неё
    // на следующем кадре, как и под высоту полного HUD.
    ui.horizontal(|ui| {
        ui.set_min_height(height);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        if let Some(clock) = extras.clock {
            strip.separate(ui);
            strip.main(ui, Some(format::clock(clock)), theme::TEXT_PRIMARY);
        }
        let blocks = &settings.blocks;
        for &kind in &blocks.order {
            let block = blocks.get(kind);
            if !block.enabled || !visible(settings, snapshot, kind, strip.warming_up) {
                continue;
            }
            strip.separate(ui);
            match kind {
                BlockKind::Gpu => gpu(ui, &strip, snapshot, block),
                BlockKind::Cpu => cpu(ui, &strip, snapshot, block),
                BlockKind::Ram => memory(ui, &strip, snapshot, block),
                BlockKind::Fps => fps(ui, &strip, snapshot, block),
            }
        }
    });
    for note in extras.notes {
        ui.label(RichText::new(note).font(theme::regular(11.0)).color(theme::TEXT_DISABLED));
    }
}

/// С «скрывать недоступное» пустой блок в полосу не попадает — как и в полном HUD.
fn visible(settings: &Settings, snapshot: &Snapshot, kind: BlockKind, warming_up: bool) -> bool {
    let has_any_value = match kind {
        BlockKind::Gpu => snapshot.gpu.has_any_value(),
        BlockKind::Cpu => snapshot.cpu.has_any_value(),
        BlockKind::Ram => snapshot.memory.has_any_value(),
        BlockKind::Fps => true,
    };
    !settings.metrics.hide_unavailable || warming_up || has_any_value
}

fn load_color(block: &Block, load: Option<f64>, normal: Color32) -> Color32 {
    if block.thresholds.color_load {
        theme::for_level(blocks::level(load, LOAD_WARN_PERCENT, LOAD_CRITICAL_PERCENT), normal)
    } else {
        normal
    }
}

fn temperature(
    strip: &Strip<'_>,
    block: &Block,
    celsius: Option<f64>,
    normal: Color32,
) -> (Option<String>, Color32) {
    let thresholds = &block.thresholds;
    let level =
        blocks::level(celsius, f64::from(thresholds.warn_c), f64::from(thresholds.critical_c));
    let unit = strip.settings.units.temperature;
    (celsius.map(|value| format::temperature(value, unit)), theme::for_level(level, normal))
}

fn gpu(ui: &mut Ui, strip: &Strip<'_>, snapshot: &Snapshot, block: &Block) {
    let palette = Palette::of(&block.colors);
    let gpu = &snapshot.gpu;
    let device = gpu.name.as_deref().map(format::short_device_name);
    strip.title(ui, &block.heading("GPU", device.as_deref()), palette.header);
    let load_color = load_color(block, gpu.load_percent, palette.header);
    strip.main(ui, gpu.load_percent.map(format::percent), load_color);
    if strip.enabled(Metric::GpuTemperature) {
        let (value, color) = temperature(strip, block, gpu.temperature_c, palette.values);
        strip.extra(ui, "", value, palette, color);
    }
    if strip.enabled(Metric::GpuPower) {
        strip.extra(ui, "", gpu.power_watts.map(format::power), palette, palette.values);
    }
    if strip.enabled(Metric::GpuVram) {
        let unit = strip.settings.units.memory;
        let vram = gpu.vram_used_bytes.map(|bytes| format::bytes(bytes, unit));
        strip.extra(ui, "VRAM", vram, palette, palette.values);
    }
}

fn cpu(ui: &mut Ui, strip: &Strip<'_>, snapshot: &Snapshot, block: &Block) {
    let palette = Palette::of(&block.colors);
    let cpu = &snapshot.cpu;
    let device = cpu.name.as_deref().map(format::short_device_name);
    strip.title(ui, &block.heading("CPU", device.as_deref()), palette.header);
    let load_color = load_color(block, cpu.load_percent, palette.header);
    strip.main(ui, cpu.load_percent.map(format::percent), load_color);
    if strip.enabled(Metric::CpuTemperature) {
        let (value, color) = temperature(strip, block, cpu.temperature_c, palette.values);
        strip.extra(ui, "", value, palette, color);
    }
    if strip.enabled(Metric::CpuPower) {
        strip.extra(ui, "", cpu.power_watts.map(format::power), palette, palette.values);
    }
}

fn memory(ui: &mut Ui, strip: &Strip<'_>, snapshot: &Snapshot, block: &Block) {
    let palette = Palette::of(&block.colors);
    let memory = &snapshot.memory;
    strip.title(ui, &block.heading("RAM", None), palette.header);
    strip.main(ui, memory.load_percent().map(format::percent), palette.header);
    if strip.enabled(Metric::RamUsed) {
        let unit = strip.settings.units.memory;
        let used = memory.used_bytes.map(|bytes| format::bytes(bytes, unit));
        strip.extra(ui, "", used, palette, palette.values);
    }
    if strip.enabled(Metric::RamProcess) {
        let unit = strip.settings.units.memory;
        let process = memory.process_bytes.map(|bytes| format::bytes(bytes, unit));
        strip.extra(ui, "App", process, palette, palette.values);
    }
}

fn fps(ui: &mut Ui, strip: &Strip<'_>, snapshot: &Snapshot, block: &Block) {
    let palette = Palette::of(&block.colors);
    let state = &snapshot.fps;
    let delivering = state.availability == FpsAvailability::Available;
    let stats = delivering.then_some(&state.statistics);
    let fps_text = |value: Option<f64>| value.and_then(format::fps);
    strip.title(ui, &block.heading("FPS", None), palette.header);
    strip.main(ui, fps_text(stats.and_then(|s| s.current_fps)), palette.accent);
    if strip.enabled(Metric::FpsAverage) {
        let average = fps_text(stats.and_then(|s| s.average_fps));
        strip.extra(ui, "AVG", average, palette, palette.values);
    }
    if strip.enabled(Metric::FpsLow1) {
        let low = fps_text(stats.and_then(|s| s.low_1_percent_fps));
        strip.extra(ui, "1%", low, palette, palette.values);
    }
    if strip.enabled(Metric::FpsLatency) {
        let latency = state.latency_ms.filter(|_| delivering).and_then(format::latency);
        strip.extra(ui, "LAT", latency, palette, palette.values);
    }
    // Почему цифр нет — коротко, прямо в полосе: отдельной строки причины здесь нет.
    if !delivering && let Some(message) = state.message() {
        strip.extra(ui, "", Some(message.to_string()), palette, theme::TEXT_DISABLED);
    }
}

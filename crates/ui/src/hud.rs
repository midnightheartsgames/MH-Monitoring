//! Весь HUD. Смотрит на один снимок и не знает, откуда взялись числа.
//!
//! Раскладка перенесена из `OverlayScreen.kt`: секции GPU / CPU / RAM / FPS, заголовок секции
//! заодно показывает её главное число. Новое — строка причины под секцией, если секция заполнена
//! не целиком: прочерк без объяснения план запрещает.

use std::collections::BTreeSet;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Layout, Margin, Pos2, Rect, RichText, Sense, Shape,
    Stroke, Ui, Vec2, pos2, vec2,
};
use mh_core::{FpsAvailability, FrametimeGraph, SectionHealth, SensorStatus, Snapshot};

use crate::blocks::{self, Block, BlockKind, LOAD_CRITICAL_PERCENT, LOAD_WARN_PERCENT};
use crate::format::{self, EMPTY, WARMING_UP};
use crate::settings::{Metric, OverlayMode, Settings};
use crate::theme::{self, Palette};

const PADDING_X: i8 = 11;
const PADDING_Y: i8 = 7;
const GRAPH_HEIGHT: f32 = 56.0;
/// Эталонная линия графика — 60 FPS.
const REFERENCE_MS: f32 = 16.67;
/// Непрозрачность заливки под линией графика.
const GRAPH_FILL_ALPHA: f32 = 34.0 / 255.0;

/// Что пользователь сделал в HUD за этот кадр.
#[derive(Debug, Default)]
pub struct HudActions {
    pub toggle_lock: bool,
    pub hide: bool,
}

/// Что HUD показывает помимо снимка — это знает приложение, а не движок.
#[derive(Debug, Clone, Copy, Default)]
pub struct Extras<'a> {
    /// Сообщения приложения внизу HUD: хоткеи, сброшенные настройки.
    pub notes: &'a [String],
    /// Размер окна игры.
    pub resolution: Option<(u32, u32)>,
    /// Местное время, если его показывать.
    pub clock: Option<(u8, u8)>,
}

/// Строки, которые хоть раз показали значение. Такая строка держит место, даже если значение
/// пропало, — иначе раскладка прыгала бы от каждого пропуска датчика.
pub type SeenRows = BTreeSet<Metric>;

struct Rows<'a> {
    settings: &'a Settings,
    seen: &'a mut SeenRows,
    warming_up: bool,
    /// Цвета блока, который сейчас рисуется.
    palette: Palette,
    /// Размер окна игры — его знает UI, а не движок.
    resolution: Option<(u32, u32)>,
}

impl Rows<'_> {
    /// Строка цветом значений блока.
    fn row(&mut self, ui: &mut Ui, metric: Metric, label: &str, value: Option<String>) {
        let color = self.palette.values;
        self.colored_row(ui, metric, label, value, color, false);
    }

    /// Строка с учётом настроек. `always_visible` — для значений, которые по природе приходят
    /// поздно (процентили ждут сотни кадров): они не должны появляться посреди сеанса.
    fn colored_row(
        &mut self,
        ui: &mut Ui,
        metric: Metric,
        label: &str,
        value: Option<String>,
        color: Color32,
        always_visible: bool,
    ) {
        if !self.settings.metrics.is_enabled(metric) {
            return;
        }
        if value.is_some() {
            self.seen.insert(metric);
        }
        let hidden = self.settings.metrics.hide_unavailable
            && !always_visible
            && !self.warming_up
            && !self.seen.contains(&metric);
        if value.is_none() && hidden {
            return;
        }
        let text = value_or_empty(value, self.warming_up);
        metric_row(ui, label, &text, self.palette.labels, color);
    }

    fn enabled(&self, metric: Metric) -> bool {
        self.settings.metrics.is_enabled(metric)
    }

    /// Начинает блок: дальше строки рисуются его цветами.
    fn begin(&mut self, block: &Block) {
        self.palette = Palette::of(&block.colors);
    }

    /// Заголовок блока с главным числом.
    fn title(&self, ui: &mut Ui, title: &str, value: &str, value_color: Color32) {
        section_title(ui, title, value, self.palette.header, value_color);
    }

    /// Показывать ли секцию. С «скрывать недоступное» пустая секция исчезает целиком.
    fn section_visible(&self, enabled: bool, has_any_value: bool) -> bool {
        enabled && (!self.settings.metrics.hide_unavailable || self.warming_up || has_any_value)
    }
}

/// Рисует HUD. Возвращает прямоугольник, который он занял, — по нему подгоняется окно.
pub fn show(
    ui: &mut Ui,
    snapshot: &Snapshot,
    settings: &Settings,
    seen: &mut SeenRows,
    extras: &Extras<'_>,
    actions: &mut HudActions,
) -> Rect {
    let overlay = &settings.overlay;
    if overlay.mode == OverlayMode::Strip {
        // В полосе нет заголовка с кнопками: замок и «скрыть» — в трее и на хоткеях.
        return egui::Frame::new()
            .fill(theme::background(overlay.opacity))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::symmetric(PADDING_X, 2))
            .show(ui, |ui| crate::strip::contents(ui, snapshot, settings, extras))
            .response
            .rect;
    }
    let notes = extras.notes;
    let frame = egui::Frame::new()
        .fill(theme::background(overlay.opacity))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(PADDING_X, PADDING_Y));
    frame
        .show(ui, |ui| {
            let width = width(settings) - 2.0 * f32::from(PADDING_X);
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let mut rows = Rows {
                settings,
                seen,
                warming_up: snapshot.is_warming_up(),
                palette: Palette::of(&Default::default()),
                resolution: extras.resolution,
            };

            if overlay.show_header && !overlay.locked {
                header(ui, actions);
            }
            let blocks = &settings.blocks;
            let mut needs_divider = false;
            let mut separate = |ui: &mut Ui| {
                if needs_divider {
                    divider(ui);
                }
                needs_divider = true;
            };
            if let Some(clock) = extras.clock {
                separate(ui);
                let color = theme::DEFAULT_HEADER;
                section_title(ui, "Time", &format::clock(clock), color, color);
            }
            for &kind in &blocks.order {
                let has_any_value = match kind {
                    BlockKind::Gpu => snapshot.gpu.has_any_value(),
                    BlockKind::Cpu => snapshot.cpu.has_any_value(),
                    BlockKind::Ram => snapshot.memory.has_any_value(),
                    // FPS не прячется: его отсутствие само — сообщение («жду игру»).
                    BlockKind::Fps => true,
                };
                if !rows.section_visible(blocks.get(kind).enabled, has_any_value) {
                    continue;
                }
                separate(ui);
                match kind {
                    BlockKind::Gpu => gpu(ui, snapshot, &mut rows),
                    BlockKind::Cpu => cpu(ui, snapshot, &mut rows),
                    BlockKind::Ram => memory(ui, snapshot, &mut rows),
                    BlockKind::Fps => fps(ui, snapshot, overlay.show_graph, &mut rows),
                }
            }
            // Сообщения самого приложения — хоткей занят, настройки сброшены и т. п.
            if !notes.is_empty() {
                separate(ui);
                for note in notes {
                    caption(ui, note);
                }
            }
        })
        .response
        .rect
}

fn header(ui: &mut Ui, actions: &mut HudActions) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 22.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(
                RichText::new("MH MONITORING")
                    .font(theme::bold(12.0))
                    .color(theme::TEXT_SECONDARY)
                    .extra_letter_spacing(1.2),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if header_button(ui, "×", "Скрыть (трей → показать)") {
                    actions.hide = true;
                }
                ui.add_space(8.0);
                if lock_button(ui).clicked() {
                    actions.toggle_lock = true;
                }
            });
        },
    );
}

fn header_button(ui: &mut Ui, text: &str, hint: &str) -> bool {
    let label = RichText::new(text).font(theme::bold(15.0)).color(theme::TEXT_SECONDARY);
    ui.add(egui::Label::new(label).sense(Sense::click())).on_hover_text(hint).clicked()
}

/// Замок рисуется сам: в Cuprum такого знака нет, а запасной emoji-шрифт выглядит чужим.
fn lock_button(ui: &mut Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::click());
    let color = if response.hovered() { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    let painter = ui.painter();
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + 6.0), rect.right_bottom());
    painter.rect_filled(body, CornerRadius::same(1), color);
    let center = pos2(rect.center().x, rect.top() + 6.0);
    let shackle: Vec<Pos2> = (0..=12)
        .map(|step| {
            let angle = std::f32::consts::PI * (1.0 + step as f32 / 12.0);
            center + Vec2::angled(angle) * 3.5
        })
        .collect();
    painter.add(Shape::line(shackle, Stroke::new(1.6, color)));
    response.on_hover_text("Заблокировать: мышь уходит игре")
}

fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 11.0), Sense::hover());
    let y = rect.center().y;
    ui.painter().hline(rect.x_range(), y, Stroke::new(1.0, theme::DIVIDER));
}

/// «GPU ........ 87%». Число рисуется первым: длинное имя устройства обрезается, а не
/// выталкивает его из строки.
fn section_title(
    ui: &mut Ui,
    title: &str,
    value: &str,
    title_color: Color32,
    value_color: Color32,
) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
            ui.label(RichText::new(value).font(theme::bold(15.0)).color(value_color));
            ui.add_space(8.0);
            ui.with_layout(Layout::left_to_right(Align::Max), |ui| {
                let text = RichText::new(title).font(theme::bold(15.0)).color(title_color);
                ui.add(egui::Label::new(text).truncate());
            });
        });
    });
}

/// «label ......... value». Значения выровнены вправо, чтобы цифры не сдвигали строку.
fn metric_row(ui: &mut Ui, label: &str, value: &str, label_color: Color32, color: Color32) {
    ui.add_space(1.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).font(theme::regular(12.5)).color(label_color));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).font(theme::regular(13.5)).color(color));
        });
    });
    ui.add_space(1.0);
}

fn caption(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text).font(theme::regular(11.0)).color(theme::TEXT_DISABLED),
        )
        .wrap(),
    );
}

/// Почему секция заполнена не целиком — одна строка, если есть что сказать.
fn health_note(ui: &mut Ui, health: SectionHealth) {
    if health.status == SensorStatus::Available {
        return;
    }
    if let Some(reason) = health.reason {
        caption(ui, reason.message());
    }
}

fn value_or_empty(value: Option<String>, warming_up: bool) -> String {
    value.unwrap_or_else(|| if warming_up { WARMING_UP } else { EMPTY }.to_string())
}

/// Заголовок блока с загрузкой; с цветовой индикацией загрузка желтеет и краснеет.
fn load_title(ui: &mut Ui, rows: &Rows<'_>, block: &Block, heading: &str, load: Option<f64>) {
    let text = value_or_empty(load.map(format::percent), rows.warming_up);
    let normal = rows.palette.header;
    let color = if block.thresholds.color_load {
        theme::for_level(blocks::level(load, LOAD_WARN_PERCENT, LOAD_CRITICAL_PERCENT), normal)
    } else {
        normal
    };
    rows.title(ui, heading, &text, color);
}

/// Ширина HUD в точках по настройке.
pub fn width(settings: &Settings) -> f32 {
    theme::OVERLAY_WIDTH * settings.overlay.width_scale
}

/// Температура строкой: цвет по порогам блока.
fn temperature_row(
    ui: &mut Ui,
    rows: &mut Rows<'_>,
    block: &Block,
    metric: Metric,
    value: Option<f64>,
) {
    let thresholds = &block.thresholds;
    let level =
        blocks::level(value, f64::from(thresholds.warn_c), f64::from(thresholds.critical_c));
    let color = theme::for_level(level, rows.palette.values);
    let unit = rows.settings.units.temperature;
    let text = value.map(|celsius| format::temperature(celsius, unit));
    rows.colored_row(ui, metric, "Temp", text, color, false);
}

fn gpu(ui: &mut Ui, snapshot: &Snapshot, rows: &mut Rows<'_>) {
    let block = &rows.settings.blocks.gpu;
    rows.begin(block);
    let gpu = &snapshot.gpu;
    let device = gpu.name.as_deref().map(format::short_device_name);
    load_title(ui, rows, block, &block.heading("GPU", device.as_deref()), gpu.load_percent);
    let units = rows.settings.units;
    temperature_row(ui, rows, block, Metric::GpuTemperature, gpu.temperature_c);
    let clock = gpu.core_clock_mhz.map(|mhz| format::frequency(mhz, units.frequency));
    rows.row(ui, Metric::GpuClock, "Clock", clock);
    let memory_clock = gpu.memory_clock_mhz.map(|mhz| format::frequency(mhz, units.frequency));
    rows.row(ui, Metric::GpuMemoryClock, "Mem Clock", memory_clock);
    let vram = format::bytes_pair(gpu.vram_used_bytes, gpu.vram_total_bytes, units.memory);
    rows.row(ui, Metric::GpuVram, "VRAM", vram);
    rows.row(ui, Metric::GpuPower, "Power", gpu.power_watts.map(format::power));
    rows.row(ui, Metric::GpuFan, "Fan", gpu.fan_rpm.map(format::rpm));
    rows.row(ui, Metric::GpuFanPercent, "Fan %", gpu.fan_percent.map(format::percent));
    health_note(ui, gpu.health);
}

fn cpu(ui: &mut Ui, snapshot: &Snapshot, rows: &mut Rows<'_>) {
    let block = &rows.settings.blocks.cpu;
    rows.begin(block);
    let cpu = &snapshot.cpu;
    let device = cpu.name.as_deref().map(format::short_device_name);
    load_title(ui, rows, block, &block.heading("CPU", device.as_deref()), cpu.load_percent);
    let units = rows.settings.units;
    temperature_row(ui, rows, block, Metric::CpuTemperature, cpu.temperature_c);
    let clock = cpu.clock_mhz.map(|mhz| format::frequency(mhz, units.frequency));
    rows.row(ui, Metric::CpuClock, "Clock", clock);
    // Строки P- и E-ядер есть только у гибридных процессоров — у прочих их нет вовсе, а не
    // прочерк.
    if let Some(hybrid) = cpu.hybrid_clocks() {
        let text = |mhz: Option<f64>| mhz.map(|mhz| format::frequency(mhz, units.frequency));
        rows.row(ui, Metric::CpuHybridClock, "P-Clock", text(hybrid.performance_mhz));
        rows.row(ui, Metric::CpuHybridClock, "E-Clock", text(hybrid.efficient_mhz));
    }
    rows.row(ui, Metric::CpuPower, "Power", cpu.power_watts.map(format::power));
    if rows.enabled(Metric::CpuCoreLoad) && !cpu.cores.is_empty() {
        ui.add_space(3.0);
        core_load_bars(ui, &cpu.cores, rows.palette);
    }
    if rows.enabled(Metric::CpuCoreClock) && cpu.cores.iter().any(|core| core.clock_mhz.is_some()) {
        ui.add_space(3.0);
        core_clock_grid(ui, &cpu.cores, units.frequency, rows.palette);
    }
    health_note(ui, cpu.health);
}

fn memory(ui: &mut Ui, snapshot: &Snapshot, rows: &mut Rows<'_>) {
    let block = &rows.settings.blocks.ram;
    rows.begin(block);
    let memory = &snapshot.memory;
    let load = value_or_empty(memory.load_percent().map(format::percent), rows.warming_up);
    rows.title(ui, &block.heading("RAM", None), &load, rows.palette.header);
    let unit = rows.settings.units.memory;
    let used = format::bytes_pair(memory.used_bytes, memory.total_bytes, unit);
    rows.row(ui, Metric::RamUsed, "Used", used);
    // Память захваченного приложения. Прочерк — цели нет или её не открыть.
    let process = memory.process_bytes.map(|bytes| format::bytes(bytes, unit));
    rows.row(ui, Metric::RamProcess, "App", process);
    rows.row(ui, Metric::RamSpeed, "Speed", memory.speed_mhz.map(format::memory_speed));
    health_note(ui, memory.health);
}

fn fps(ui: &mut Ui, snapshot: &Snapshot, show_graph: bool, rows: &mut Rows<'_>) {
    let block = &rows.settings.blocks.fps;
    rows.begin(block);
    let palette = rows.palette;
    let state = &snapshot.fps;
    // Цифры показываются, только пока источник их действительно даёт: устаревшее среднее хуже
    // прочерка.
    let delivering = state.availability == FpsAvailability::Available;
    let stats = if delivering { Some(&state.statistics) } else { None };
    let fps_text = |value: Option<f64>| value.and_then(format::fps);

    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
            let current = fps_text(stats.and_then(|s| s.current_fps));
            ui.label(
                RichText::new(value_or_empty(current, rows.warming_up))
                    .font(theme::bold(28.0))
                    .color(palette.accent),
            );
            ui.add_space(8.0);
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                let heading = block.heading("FPS", None);
                let text = RichText::new(heading).font(theme::bold(15.0)).color(palette.header);
                ui.add(egui::Label::new(text).truncate());
            });
        });
    });
    // Три строки ведут себя одинаково: значение, которое ещё набирается, не повод исчезнуть.
    let average = fps_text(stats.and_then(|s| s.average_fps));
    rows.colored_row(ui, Metric::FpsAverage, "AVG", average, palette.values, true);
    let low_1 = fps_text(stats.and_then(|s| s.low_1_percent_fps));
    rows.colored_row(ui, Metric::FpsLow1, "1% LOW", low_1, palette.values, true);
    let low_01 = fps_text(stats.and_then(|s| s.low_0_1_percent_fps));
    rows.colored_row(ui, Metric::FpsLow01, "0.1% LOW", low_01, palette.values, true);
    let latency = state.latency_ms.filter(|_| delivering).and_then(format::latency);
    rows.row(ui, Metric::FpsLatency, "LAT", latency);
    let target_known = state.target.is_some();
    let api = state.api().filter(|_| delivering);
    let resolution = rows.resolution.filter(|_| target_known);
    rows.row(ui, Metric::FpsApi, "API", format::api_and_resolution(api, resolution));

    // Чьи это цифры и почему их нет, когда их нет. Без этой строки живое, устаревшее и
    // отсутствующее значение выглядят одинаково. Имя процесса можно убрать — причину нет.
    let summary = if rows.enabled(Metric::FpsTarget) {
        state.summary()
    } else {
        state.message().map(str::to_string)
    };
    if let Some(summary) = summary {
        ui.add_space(1.0);
        caption(ui, &summary);
    }

    if show_graph {
        ui.add_space(6.0);
        graph(ui, &snapshot.frametime_graph, palette.accent);
        ui.add_space(2.0);
        let frametime = stats.and_then(|s| s.current_frametime_ms).and_then(format::frametime);
        ui.label(
            RichText::new(value_or_empty(frametime, false))
                .font(theme::regular(11.5))
                .color(palette.labels),
        );
    }
}

const CORE_BARS_HEIGHT: f32 = 26.0;
const CORE_GRID_COLUMNS: usize = 4;

/// Загрузка каждого логического ядра столбиком. E-ядра бледнее P-ядер.
fn core_load_bars(ui: &mut Ui, cores: &[mh_core::CoreStats], palette: Palette) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), CORE_BARS_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    let top_class = cores.iter().map(|core| core.efficiency_class).max().unwrap_or(0);
    let gap = 1.0;
    let width = (rect.width() - gap * (cores.len() as f32 - 1.0)) / cores.len() as f32;
    for (index, core) in cores.iter().enumerate() {
        let left = rect.left() + index as f32 * (width + gap);
        let track = Rect::from_min_size(pos2(left, rect.top()), vec2(width, rect.height()));
        painter.rect_filled(track, CornerRadius::ZERO, theme::GRAPH_REFERENCE);
        let Some(load) = core.load_percent else { continue };
        let height = rect.height() * (load as f32 / 100.0).clamp(0.0, 1.0);
        let bar = Rect::from_min_max(pos2(left, rect.bottom() - height), track.right_bottom());
        let color = if core.efficiency_class < top_class {
            palette.accent.gamma_multiply(0.55)
        } else {
            palette.accent
        };
        painter.rect_filled(bar, CornerRadius::ZERO, color);
    }
}

/// Частоты ядер сеткой по четыре в ряд.
fn core_clock_grid(
    ui: &mut Ui,
    cores: &[mh_core::CoreStats],
    unit: crate::units::Frequency,
    palette: Palette,
) {
    let label = match unit {
        crate::units::Frequency::Mhz => "Cores, MHz",
        _ => "Cores, GHz",
    };
    ui.label(RichText::new(label).font(theme::regular(11.0)).color(palette.labels));
    let column = ui.available_width() / CORE_GRID_COLUMNS as f32;
    for chunk in cores.chunks(CORE_GRID_COLUMNS) {
        ui.horizontal(|ui| {
            for core in chunk {
                let text = core
                    .clock_mhz
                    .map_or_else(|| EMPTY.to_string(), |mhz| format::core_clock(mhz, unit));
                let (cell, _) = ui.allocate_exact_size(vec2(column, 14.0), Sense::hover());
                ui.painter().text(
                    cell.right_center() - vec2(4.0, 0.0),
                    Align2::RIGHT_CENTER,
                    text,
                    theme::regular(11.5),
                    palette.values,
                );
            }
        });
    }
}

/// Столбцы — пики frametime; всё выше потолка срезано и помечено красной риской.
fn graph(ui: &mut Ui, graph: &FrametimeGraph, color: Color32) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), GRAPH_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    let ceiling = graph.ceiling_ms.max(1.0);
    let y = |ms: f32| rect.bottom() - (ms.min(ceiling) / ceiling) * rect.height();

    if REFERENCE_MS < ceiling {
        painter.hline(rect.x_range(), y(REFERENCE_MS), Stroke::new(1.0, theme::GRAPH_REFERENCE));
    }
    painter.text(
        rect.left_top(),
        Align2::LEFT_TOP,
        format!("{ceiling:.0} ms"),
        theme::regular(10.0),
        theme::TEXT_DISABLED,
    );

    let count = graph.columns.len();
    if count < 2 {
        return;
    }
    let step = rect.width() / (count - 1) as f32;
    let x = |index: usize| rect.left() + index as f32 * step;

    // Заливка — тонкими прямоугольниками: egui заливает только выпуклые многоугольники.
    // Цвет — тот же, что у линии, только почти прозрачный.
    let fill_color = color.gamma_multiply(GRAPH_FILL_ALPHA);
    for (index, &value) in graph.columns.iter().enumerate() {
        if value <= 0.0 {
            continue;
        }
        let left = x(index) - step / 2.0;
        let fill = Rect::from_min_max(pos2(left, y(value)), pos2(left + step, rect.bottom()));
        painter.rect_filled(fill.intersect(rect), CornerRadius::ZERO, fill_color);
    }

    // Линия рвётся там, где кадров не было: соединять через пустоту — выдумывать данные.
    let mut run: Vec<Pos2> = Vec::new();
    let line = Stroke::new(1.4, color);
    for (index, &value) in graph.columns.iter().enumerate() {
        if value > 0.0 {
            run.push(pos2(x(index), y(value)));
            if graph.is_clipped(value) {
                let top = pos2(x(index), rect.top());
                painter.line_segment(
                    [top, top + Vec2::new(0.0, 4.0)],
                    Stroke::new(2.0, theme::ACCENT_CRITICAL),
                );
            }
        } else if !run.is_empty() {
            painter.add(Shape::line(std::mem::take(&mut run), line));
        }
    }
    if run.len() > 1 {
        painter.add(Shape::line(run, line));
    }
}

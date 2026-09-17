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

use crate::format::{self, EMPTY, WARMING_UP};
use crate::settings::{Metric, Settings};
use crate::theme;

const PADDING_X: i8 = 11;
const PADDING_Y: i8 = 7;
const GRAPH_HEIGHT: f32 = 56.0;
/// Эталонная линия графика — 60 FPS.
const REFERENCE_MS: f32 = 16.67;
const PRIMARY: Color32 = theme::TEXT_PRIMARY;

/// Что пользователь сделал в HUD за этот кадр.
#[derive(Debug, Default)]
pub struct HudActions {
    pub toggle_lock: bool,
    pub hide: bool,
}

/// Строки, которые хоть раз показали значение. Такая строка держит место, даже если значение
/// пропало, — иначе раскладка прыгала бы от каждого пропуска датчика.
pub type SeenRows = BTreeSet<Metric>;

struct Rows<'a> {
    settings: &'a Settings,
    seen: &'a mut SeenRows,
    warming_up: bool,
}

impl Rows<'_> {
    /// Строка с учётом настроек. `always_visible` — для значений, которые по природе приходят
    /// поздно (процентили ждут сотни кадров): они не должны появляться посреди сеанса.
    fn row(
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
        metric_row(ui, label, &text, color);
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
    notes: &[String],
    actions: &mut HudActions,
) -> Rect {
    let overlay = &settings.overlay;
    let frame = egui::Frame::new()
        .fill(theme::background(overlay.opacity))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(PADDING_X, PADDING_Y));
    frame
        .show(ui, |ui| {
            let width = theme::OVERLAY_WIDTH - 2.0 * f32::from(PADDING_X);
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let mut rows = Rows { settings, seen, warming_up: snapshot.is_warming_up() };

            if overlay.show_header && !overlay.locked {
                header(ui, actions);
            }
            let sections = &settings.sections;
            let mut needs_divider = false;
            let mut separate = |ui: &mut Ui| {
                if needs_divider {
                    divider(ui);
                }
                needs_divider = true;
            };
            if rows.section_visible(sections.gpu, snapshot.gpu.has_any_value()) {
                separate(ui);
                gpu(ui, snapshot, &mut rows);
            }
            if rows.section_visible(sections.cpu, snapshot.cpu.has_any_value()) {
                separate(ui);
                cpu(ui, snapshot, &mut rows);
            }
            if rows.section_visible(sections.ram, snapshot.memory.has_any_value()) {
                separate(ui);
                memory(ui, snapshot, &mut rows);
            }
            if sections.fps {
                separate(ui);
                fps(ui, snapshot, overlay.show_graph, &mut rows);
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

/// «GPU ........ 87%».
fn section_title(ui: &mut Ui, title: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(theme::bold(15.0)).color(theme::TEXT_PRIMARY));
        ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
            ui.label(RichText::new(value).font(theme::bold(15.0)).color(theme::TEXT_PRIMARY));
        });
    });
}

/// «label ......... value». Значения выровнены вправо, чтобы цифры не сдвигали строку.
fn metric_row(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    ui.add_space(1.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).font(theme::regular(12.5)).color(theme::TEXT_SECONDARY));
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

fn gpu(ui: &mut Ui, snapshot: &Snapshot, rows: &mut Rows<'_>) {
    let gpu = &snapshot.gpu;
    let load = value_or_empty(gpu.load_percent.map(format::percent), rows.warming_up);
    section_title(ui, "GPU", &load);
    let temperature_color = theme::for_temperature(gpu.temperature_c);
    let temperature = gpu.temperature_c.map(format::temperature);
    rows.row(ui, Metric::GpuTemperature, "Temp", temperature, temperature_color, false);
    let clock = gpu.core_clock_mhz.map(format::frequency);
    rows.row(ui, Metric::GpuClock, "Clock", clock, PRIMARY, false);
    let vram = format::bytes_pair(gpu.vram_used_bytes, gpu.vram_total_bytes);
    rows.row(ui, Metric::GpuVram, "VRAM", vram, PRIMARY, false);
    rows.row(ui, Metric::GpuPower, "Power", gpu.power_watts.map(format::power), PRIMARY, false);
    rows.row(ui, Metric::GpuFan, "Fan", gpu.fan_rpm.map(format::rpm), PRIMARY, false);
    health_note(ui, gpu.health);
}

fn cpu(ui: &mut Ui, snapshot: &Snapshot, rows: &mut Rows<'_>) {
    let cpu = &snapshot.cpu;
    let load = value_or_empty(cpu.load_percent.map(format::percent), rows.warming_up);
    section_title(ui, "CPU", &load);
    let temperature_color = theme::for_temperature(cpu.temperature_c);
    let temperature = cpu.temperature_c.map(format::temperature);
    rows.row(ui, Metric::CpuTemperature, "Temp", temperature, temperature_color, false);
    rows.row(ui, Metric::CpuClock, "Clock", cpu.clock_mhz.map(format::frequency), PRIMARY, false);
    rows.row(ui, Metric::CpuPower, "Power", cpu.power_watts.map(format::power), PRIMARY, false);
    health_note(ui, cpu.health);
}

fn memory(ui: &mut Ui, snapshot: &Snapshot, rows: &mut Rows<'_>) {
    let memory = &snapshot.memory;
    let load = value_or_empty(memory.load_percent().map(format::percent), rows.warming_up);
    section_title(ui, "RAM", &load);
    let used = format::bytes_pair(memory.used_bytes, memory.total_bytes);
    rows.row(ui, Metric::RamUsed, "Used", used, PRIMARY, false);
    health_note(ui, memory.health);
}

fn fps(ui: &mut Ui, snapshot: &Snapshot, show_graph: bool, rows: &mut Rows<'_>) {
    let state = &snapshot.fps;
    // Цифры показываются, только пока источник их действительно даёт: устаревшее среднее хуже
    // прочерка.
    let delivering = state.availability == FpsAvailability::Available;
    let stats = if delivering { Some(&state.statistics) } else { None };
    let fps_text = |value: Option<f64>| value.and_then(format::fps);

    ui.horizontal(|ui| {
        ui.label(RichText::new("FPS").font(theme::bold(15.0)).color(theme::TEXT_PRIMARY));
        ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
            let current = fps_text(stats.and_then(|s| s.current_fps));
            ui.label(
                RichText::new(value_or_empty(current, rows.warming_up))
                    .font(theme::bold(28.0))
                    .color(theme::ACCENT),
            );
        });
    });
    // Три строки ведут себя одинаково: значение, которое ещё набирается, не повод исчезнуть.
    let average = fps_text(stats.and_then(|s| s.average_fps));
    rows.row(ui, Metric::FpsAverage, "AVG", average, PRIMARY, true);
    let low_1 = fps_text(stats.and_then(|s| s.low_1_percent_fps));
    rows.row(ui, Metric::FpsLow1, "1% LOW", low_1, PRIMARY, true);
    let low_01 = fps_text(stats.and_then(|s| s.low_0_1_percent_fps));
    rows.row(ui, Metric::FpsLow01, "0.1% LOW", low_01, PRIMARY, true);

    // Чьи это цифры и почему их нет, когда их нет. Без этой строки живое, устаревшее и
    // отсутствующее значение выглядят одинаково.
    if let Some(summary) = state.summary() {
        ui.add_space(1.0);
        caption(ui, &summary);
    }

    if show_graph {
        ui.add_space(6.0);
        graph(ui, &snapshot.frametime_graph);
        ui.add_space(2.0);
        let frametime = stats.and_then(|s| s.current_frametime_ms).and_then(format::frametime);
        ui.label(
            RichText::new(value_or_empty(frametime, false))
                .font(theme::regular(11.5))
                .color(theme::TEXT_SECONDARY),
        );
    }
}

/// Столбцы — пики frametime; всё выше потолка срезано и помечено красной риской.
fn graph(ui: &mut Ui, graph: &FrametimeGraph) {
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
    for (index, &value) in graph.columns.iter().enumerate() {
        if value <= 0.0 {
            continue;
        }
        let left = x(index) - step / 2.0;
        let fill = Rect::from_min_max(pos2(left, y(value)), pos2(left + step, rect.bottom()));
        painter.rect_filled(fill.intersect(rect), CornerRadius::ZERO, theme::GRAPH_FILL);
    }

    // Линия рвётся там, где кадров не было: соединять через пустоту — выдумывать данные.
    let mut run: Vec<Pos2> = Vec::new();
    let line = Stroke::new(1.4, theme::ACCENT);
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

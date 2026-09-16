//! Весь HUD. Смотрит на один снимок и не знает, откуда взялись числа.
//!
//! Раскладка перенесена из `OverlayScreen.kt`: секции GPU / CPU / RAM / FPS, заголовок секции
//! заодно показывает её главное число. Новое — строка причины под секцией, если секция заполнена
//! не целиком: прочерк без объяснения план запрещает.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Layout, Margin, Pos2, Rect, RichText, Sense, Shape,
    Stroke, Ui, Vec2, pos2, vec2,
};
use mh_core::{FpsAvailability, FrametimeGraph, SectionHealth, SensorStatus, Snapshot};

use crate::format::{self, EMPTY, WARMING_UP};
use crate::settings::OverlaySettings;
use crate::theme;

const PADDING_X: i8 = 11;
const PADDING_Y: i8 = 7;
const GRAPH_HEIGHT: f32 = 56.0;
/// Эталонная линия графика — 60 FPS.
const REFERENCE_MS: f32 = 16.67;

/// Что пользователь сделал в HUD за этот кадр.
#[derive(Debug, Default)]
pub struct HudActions {
    pub toggle_lock: bool,
    pub hide: bool,
}

/// Рисует HUD. Возвращает прямоугольник, который он занял, — по нему подгоняется окно.
pub fn show(
    ui: &mut Ui,
    snapshot: &Snapshot,
    settings: &OverlaySettings,
    notes: &[String],
    actions: &mut HudActions,
) -> Rect {
    let frame = egui::Frame::new()
        .fill(theme::background(settings.opacity))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(PADDING_X, PADDING_Y));
    frame
        .show(ui, |ui| {
            let width = theme::OVERLAY_WIDTH - 2.0 * f32::from(PADDING_X);
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let warming_up = snapshot.is_warming_up();

            if settings.show_header && !settings.locked {
                header(ui, actions);
            }
            gpu(ui, snapshot, warming_up);
            divider(ui);
            cpu(ui, snapshot, warming_up);
            divider(ui);
            memory(ui, snapshot, warming_up);
            divider(ui);
            fps(ui, snapshot, settings.show_graph, warming_up);
            // Сообщения самого приложения — например, хоткей занят другой программой.
            if !notes.is_empty() {
                divider(ui);
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
                RichText::new("MH MONITOR")
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
fn metric(ui: &mut Ui, label: &str, value: &str, color: Color32) {
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

fn gpu(ui: &mut Ui, snapshot: &Snapshot, warming_up: bool) {
    let gpu = &snapshot.gpu;
    let v = |value: Option<String>| value_or_empty(value, warming_up);
    section_title(ui, "GPU", &v(gpu.load_percent.map(format::percent)));
    metric(
        ui,
        "Temp",
        &v(gpu.temperature_c.map(format::temperature)),
        theme::for_temperature(gpu.temperature_c),
    );
    metric(ui, "Clock", &v(gpu.core_clock_mhz.map(format::frequency)), theme::TEXT_PRIMARY);
    metric(
        ui,
        "VRAM",
        &v(format::bytes_pair(gpu.vram_used_bytes, gpu.vram_total_bytes)),
        theme::TEXT_PRIMARY,
    );
    metric(ui, "Power", &v(gpu.power_watts.map(format::power)), theme::TEXT_PRIMARY);
    metric(ui, "Fan", &v(gpu.fan_rpm.map(format::rpm)), theme::TEXT_PRIMARY);
    health_note(ui, gpu.health);
}

fn cpu(ui: &mut Ui, snapshot: &Snapshot, warming_up: bool) {
    let cpu = &snapshot.cpu;
    let v = |value: Option<String>| value_or_empty(value, warming_up);
    section_title(ui, "CPU", &v(cpu.load_percent.map(format::percent)));
    metric(
        ui,
        "Temp",
        &v(cpu.temperature_c.map(format::temperature)),
        theme::for_temperature(cpu.temperature_c),
    );
    metric(ui, "Clock", &v(cpu.clock_mhz.map(format::frequency)), theme::TEXT_PRIMARY);
    metric(ui, "Power", &v(cpu.power_watts.map(format::power)), theme::TEXT_PRIMARY);
    health_note(ui, cpu.health);
}

fn memory(ui: &mut Ui, snapshot: &Snapshot, warming_up: bool) {
    let memory = &snapshot.memory;
    let v = |value: Option<String>| value_or_empty(value, warming_up);
    section_title(ui, "RAM", &v(memory.load_percent().map(format::percent)));
    metric(
        ui,
        "Used",
        &v(format::bytes_pair(memory.used_bytes, memory.total_bytes)),
        theme::TEXT_PRIMARY,
    );
    health_note(ui, memory.health);
}

fn fps(ui: &mut Ui, snapshot: &Snapshot, show_graph: bool, warming_up: bool) {
    let state = &snapshot.fps;
    // Цифры показываются, только пока источник их действительно даёт: устаревшее среднее хуже
    // прочерка.
    let delivering = state.availability == FpsAvailability::Available;
    let stats = if delivering { Some(&state.statistics) } else { None };
    let v = |value: Option<f64>| value_or_empty(value.and_then(format::fps), false);

    ui.horizontal(|ui| {
        ui.label(RichText::new("FPS").font(theme::bold(15.0)).color(theme::TEXT_PRIMARY));
        ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
            let current = stats.and_then(|s| s.current_fps).and_then(format::fps);
            ui.label(
                RichText::new(value_or_empty(current, warming_up))
                    .font(theme::bold(28.0))
                    .color(theme::ACCENT),
            );
        });
    });
    // Три строки ведут себя одинаково: значение, которое ещё набирается, не повод исчезнуть.
    metric(ui, "AVG", &v(stats.and_then(|s| s.average_fps)), theme::TEXT_PRIMARY);
    metric(ui, "1% LOW", &v(stats.and_then(|s| s.low_1_percent_fps)), theme::TEXT_PRIMARY);
    metric(ui, "0.1% LOW", &v(stats.and_then(|s| s.low_0_1_percent_fps)), theme::TEXT_PRIMARY);

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

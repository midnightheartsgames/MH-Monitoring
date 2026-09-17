//! Элементы окна настроек: карточка, строка «подпись — управление», тумблер, подсказка «?».
//!
//! Все строки карточки устроены одинаково: подпись слева, управление в колонке
//! [`CONTROL_COLUMN`], тумблеры прижаты к правому краю. Тогда страница читается как таблица,
//! а не как набор разнокалиберных флажков.

use std::ops::RangeInclusive;

use eframe::egui::{
    self, Align, Color32, CornerRadius, DragValue, Layout, Margin, Response, RichText, Sense,
    Slider, Stroke, Ui, vec2,
};

use crate::blocks::Rgb;
use crate::theme;

/// Где начинается колонка управления, в точках от левого края карточки.
const CONTROL_COLUMN: f32 = 210.0;
const ROW_HEIGHT: f32 = 28.0;
const SLIDER_WIDTH: f32 = 170.0;

/// Заголовок страницы.
pub fn page_title(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).font(theme::bold(22.0)).color(theme::TEXT_PRIMARY));
    ui.add_space(8.0);
}

/// Карточка с заголовком — группа связанных настроек.
pub fn card<R>(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let inner = egui::Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::CARD_STROKE))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).font(theme::bold(15.0)).color(theme::TEXT_PRIMARY));
            ui.add_space(6.0);
            add_contents(ui)
        })
        .inner;
    ui.add_space(10.0);
    inner
}

/// Строка с подписью и управлением в общей колонке. `help` — текст за значком «?».
pub fn row<R>(
    ui: &mut Ui,
    label: &str,
    help: Option<&str>,
    add_control: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), ROW_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            let start = ui.cursor().left();
            row_label(ui, label, help, true);
            let used = ui.cursor().left() - start;
            ui.add_space((CONTROL_COLUMN - used).max(8.0));
            add_control(ui)
        },
    )
    .inner
}

/// Строка с тумблером у правого края. Возвращает ответ тумблера.
pub fn switch_row(ui: &mut Ui, label: &str, help: Option<&str>, value: &mut bool) -> Response {
    switch_row_enabled(ui, label, help, value, true)
}

/// То же, но строку можно показать недоступной — например, пока выключен её раздел.
pub fn switch_row_enabled(
    ui: &mut Ui,
    label: &str,
    help: Option<&str>,
    value: &mut bool,
    enabled: bool,
) -> Response {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), ROW_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            row_label(ui, label, help, enabled);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled(enabled, Switch(value))
            })
            .inner
        },
    )
    .inner
}

fn row_label(ui: &mut Ui, label: &str, help: Option<&str>, enabled: bool) {
    let color = if enabled { theme::TEXT_PRIMARY } else { theme::TEXT_DISABLED };
    ui.label(RichText::new(label).font(theme::regular(14.0)).color(color));
    if let Some(help) = help {
        ui.add_space(4.0);
        help_icon(ui, help);
    }
}

/// Кружок «?», по наведению — пояснение.
pub fn help_icon(ui: &mut Ui, text: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let color = if response.hovered() { theme::ACCENT } else { theme::TEXT_SECONDARY };
    let painter = ui.painter();
    painter.circle_stroke(rect.center(), 7.0, Stroke::new(1.2, color));
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, "?", theme::bold(11.0), color);
    response.on_hover_text(text)
}

/// Как показывать число в поле рядом с ползунком.
#[derive(Debug, Clone, Copy)]
pub enum Unit {
    /// Значение хранится долей, показывается в процентах.
    Percent,
    /// Значение как есть, с подписью после числа.
    Plain(&'static str),
}

/// Ползунок и поле с числом рядом.
pub fn slider_with_field(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    step: f32,
    unit: Unit,
) -> Response {
    let slider = ui.add_sized(
        [SLIDER_WIDTH, ROW_HEIGHT],
        Slider::new(value, range.clone()).step_by(f64::from(step)).show_value(false),
    );
    ui.add_space(8.0);
    let field = match unit {
        Unit::Percent => {
            let (low, high) = (*range.start() * 100.0, *range.end() * 100.0);
            let mut shown = (*value * 100.0).round();
            let response = ui.add_sized(
                [64.0, 22.0],
                DragValue::new(&mut shown).range(low..=high).speed(1.0).suffix("%"),
            );
            if response.changed() {
                *value = (shown / 100.0 / step).round() * step;
            }
            response
        }
        Unit::Plain(suffix) => ui.add_sized(
            [64.0, 22.0],
            DragValue::new(value).range(range).speed(step).max_decimals(0).suffix(suffix),
        ),
    };
    slider | field
}

/// Строка выбора цвета: образец с палитрой, HEX и возврат к цвету темы. `None` — цвет темы.
pub fn color_row(ui: &mut Ui, label: &str, value: &mut Option<Rgb>, theme_color: Color32) {
    row(ui, label, None, |ui| {
        let current = value.unwrap_or_else(|| theme::to_rgb(theme_color));
        // Пока поле в фокусе, в нём то, что набирают; иначе — текущий цвет.
        let id = ui.id().with(label).with("hex");
        let mut text = ui.data(|data| data.get_temp::<String>(id)).unwrap_or_else(|| current.hex());
        let field = ui.add(
            egui::TextEdit::singleline(&mut text)
                .desired_width(84.0)
                .char_limit(7)
                .font(egui::TextStyle::Monospace),
        );
        if field.has_focus() {
            if let Some(parsed) = Rgb::parse(&text) {
                *value = Some(parsed);
            }
            ui.data_mut(|data| data.insert_temp(id, text));
        } else {
            ui.data_mut(|data| data.remove::<String>(id));
        }
        ui.add_space(6.0);
        let mut picked = current.0;
        if egui::color_picker::color_edit_button_srgb(ui, &mut picked).changed() {
            *value = Some(Rgb(picked));
        }
        ui.add_space(6.0);
        if value.is_some() {
            let reset = ui.add(egui::Button::new("↺").small().frame(false));
            if reset.on_hover_text("Цвет темы").clicked() {
                *value = None;
            }
        } else {
            ui.label(
                RichText::new("цвет темы").font(theme::regular(12.0)).color(theme::TEXT_DISABLED),
            );
        }
    });
}

/// Пояснение в рамке — для боковых панелей.
pub fn note_box(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::CARD)
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(text).font(theme::regular(12.5)).color(theme::TEXT_SECONDARY),
                )
                .wrap(),
            );
        });
}

/// Кнопка в стиле окна: основная — залитая акцентом.
pub fn button(ui: &mut Ui, text: &str, primary: bool) -> Response {
    let (fill, color) = if primary {
        (theme::ACCENT, theme::BACKGROUND)
    } else {
        (theme::FIELD, theme::TEXT_PRIMARY)
    };
    ui.add(
        egui::Button::new(RichText::new(text).font(theme::regular(14.0)).color(color))
            .fill(fill)
            .stroke(Stroke::new(1.0, theme::CARD_STROKE))
            .corner_radius(CornerRadius::same(4))
            .min_size(vec2(120.0, 30.0)),
    )
}

/// Тумблер «вкл/выкл». Ведёт себя как флажок: щелчок меняет значение.
pub struct Switch<'a>(pub &'a mut bool);

impl egui::Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = vec2(38.0, 20.0);
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
        if response.clicked() {
            *self.0 = !*self.0;
            response.mark_changed();
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *self.0, "")
        });
        if ui.is_rect_visible(rect) {
            let progress = ui.ctx().animate_bool_responsive(response.id, *self.0);
            let track = lerp_color(theme::SWITCH_OFF, theme::ACCENT, progress);
            let track = if ui.is_enabled() { track } else { track.gamma_multiply(0.4) };
            let radius = rect.height() / 2.0;
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(radius as u8), track);
            let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), progress);
            let knob = if response.hovered() { Color32::WHITE } else { theme::TEXT_PRIMARY };
            painter.circle_filled(egui::pos2(x, rect.center().y), radius - 3.0, knob);
        }
        response
    }
}

fn lerp_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgb(mix(from.r(), to.r()), mix(from.g(), to.g()), mix(from.b(), to.b()))
}

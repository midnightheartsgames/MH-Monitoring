//! Палитра и шрифты — перенесены из `AppColors.kt` и `AppTypography.kt` как есть.

use eframe::egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

use crate::blocks::{BlockColors, Level, Rgb};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x0B, 0x0B, 0x0D);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xF1, 0xF1, 0xF1);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xA9, 0xA9, 0xAD);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x6A, 0x6A, 0x70);
pub const DIVIDER: Color32 = Color32::from_rgba_premultiplied(0x1F, 0x1F, 0x1F, 0x1F);
pub const ACCENT: Color32 = Color32::from_rgb(0x3F, 0xD0, 0xD8);
pub const ACCENT_WARN: Color32 = Color32::from_rgb(0xF2, 0xA3, 0x3C);
pub const ACCENT_CRITICAL: Color32 = Color32::from_rgb(0xE8, 0x5C, 0x5C);
pub const GRAPH_REFERENCE: Color32 = Color32::from_rgba_premultiplied(0x33, 0x33, 0x33, 0x33);

/// Окно настроек: фон, боковые панели, карточки и поля.
pub const WINDOW_BACKGROUND: Color32 = Color32::from_rgb(0x0E, 0x11, 0x16);
pub const WINDOW_PANEL: Color32 = Color32::from_rgb(0x12, 0x16, 0x1D);
pub const CARD: Color32 = Color32::from_rgb(0x15, 0x1A, 0x22);
pub const CARD_STROKE: Color32 = Color32::from_rgb(0x24, 0x2C, 0x38);
pub const FIELD: Color32 = Color32::from_rgb(0x0F, 0x13, 0x19);
pub const SWITCH_OFF: Color32 = Color32::from_rgb(0x3A, 0x42, 0x50);

/// Ширина HUD в точках до масштаба.
pub const OVERLAY_WIDTH: f32 = 272.0;

/// Цвета блока HUD по умолчанию — ими же окно настроек показывает «цвет темы».
pub const DEFAULT_HEADER: Color32 = TEXT_PRIMARY;
pub const DEFAULT_LABELS: Color32 = TEXT_SECONDARY;
pub const DEFAULT_VALUES: Color32 = TEXT_PRIMARY;
pub const DEFAULT_ACCENT: Color32 = ACCENT;

/// Цвета одного блока HUD после настроек.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub header: Color32,
    pub labels: Color32,
    pub values: Color32,
    pub accent: Color32,
}

impl Palette {
    pub fn of(colors: &BlockColors) -> Palette {
        let pick = |color: Option<Rgb>, default: Color32| color.map_or(default, rgb);
        Palette {
            header: pick(colors.header, DEFAULT_HEADER),
            labels: pick(colors.labels, DEFAULT_LABELS),
            values: pick(colors.values, DEFAULT_VALUES),
            accent: pick(colors.accent, DEFAULT_ACCENT),
        }
    }
}

pub fn rgb(color: Rgb) -> Color32 {
    let [r, g, b] = color.0;
    Color32::from_rgb(r, g, b)
}

pub fn to_rgb(color: Color32) -> Rgb {
    Rgb([color.r(), color.g(), color.b()])
}

/// Цвет значения по порогам: предупреждение, перегрев или обычный цвет.
pub fn for_level(level: Option<Level>, normal: Color32) -> Color32 {
    match level {
        Some(Level::Critical) => ACCENT_CRITICAL,
        Some(Level::Warn) => ACCENT_WARN,
        None => normal,
    }
}

/// Фон с непрозрачностью из настроек. Текст рисуется своими цветами и не бледнеет вместе с фоном —
/// в старом проекте альфа висела на всём контейнере, и это была ошибка.
pub fn background(opacity: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        BACKGROUND.r(),
        BACKGROUND.g(),
        BACKGROUND.b(),
        (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

const BOLD: &str = "cuprum-bold";

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

/// Cuprum первым в семействе; шрифты egui остаются запасными для символов, которых в Cuprum нет.
pub fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "cuprum".into(),
        Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Cuprum-Regular.ttf"))),
    );
    fonts.font_data.insert(
        BOLD.into(),
        Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Cuprum-Bold.ttf"))),
    );
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut regular = vec!["cuprum".to_string()];
    regular.extend(fallback.iter().cloned());
    let mut bold = vec![BOLD.to_string()];
    bold.extend(fallback);
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts.families.insert(FontFamily::Name(BOLD.into()), bold);
    fonts
}

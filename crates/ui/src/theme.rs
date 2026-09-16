//! Палитра и шрифты — перенесены из `AppColors.kt` и `AppTypography.kt` как есть.

use eframe::egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

pub const BACKGROUND: Color32 = Color32::from_rgb(0x0B, 0x0B, 0x0D);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xF1, 0xF1, 0xF1);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xA9, 0xA9, 0xAD);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x6A, 0x6A, 0x70);
pub const DIVIDER: Color32 = Color32::from_rgba_premultiplied(0x1F, 0x1F, 0x1F, 0x1F);
pub const ACCENT: Color32 = Color32::from_rgb(0x3F, 0xD0, 0xD8);
pub const ACCENT_WARN: Color32 = Color32::from_rgb(0xF2, 0xA3, 0x3C);
pub const ACCENT_CRITICAL: Color32 = Color32::from_rgb(0xE8, 0x5C, 0x5C);
pub const GRAPH_FILL: Color32 = Color32::from_rgba_premultiplied(0x08, 0x1C, 0x1D, 0x22);
pub const GRAPH_REFERENCE: Color32 = Color32::from_rgba_premultiplied(0x33, 0x33, 0x33, 0x33);

/// Ширина HUD в точках до масштаба.
pub const OVERLAY_WIDTH: f32 = 272.0;

/// Цвет перегрева — только там, где он действительно важен игроку.
pub fn for_temperature(celsius: Option<f64>) -> Color32 {
    match celsius {
        Some(value) if value >= 90.0 => ACCENT_CRITICAL,
        Some(value) if value >= 80.0 => ACCENT_WARN,
        _ => TEXT_PRIMARY,
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

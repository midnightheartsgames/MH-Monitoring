//! Где стоит HUD, закреплённый на мониторе: выбор монитора и угол с отступами.
//!
//! Чистая арифметика в физических пикселях — окна здесь не трогаются. Так правило проверяется
//! тестами, а слой окна только переставляет HUD туда, куда оно сказало.

use serde::{Deserialize, Serialize};

/// Прямоугольник в физических пикселях экрана.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Area {
    pub fn width(self) -> i32 {
        self.right - self.left
    }

    pub fn height(self) -> i32 {
        self.bottom - self.top
    }
}

/// Угол или середина края, к которому прижат HUD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 6] = [
        Corner::TopLeft,
        Corner::TopCenter,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomCenter,
        Corner::BottomRight,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Corner::TopLeft => "Сверху слева",
            Corner::TopCenter => "Сверху по центру",
            Corner::TopRight => "Сверху справа",
            Corner::BottomLeft => "Снизу слева",
            Corner::BottomCenter => "Снизу по центру",
            Corner::BottomRight => "Снизу справа",
        }
    }
}

/// Пределы отступа от края, в пикселях.
pub const MAX_OFFSET: i32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Anchor {
    /// Закреплён — HUD стоит в углу монитора и не перетаскивается.
    pub enabled: bool,
    /// 0 — основной монитор, 1 и дальше — монитор по порядку, как его перечисляет Windows.
    pub monitor: usize,
    #[serde(deserialize_with = "crate::settings::lenient")]
    pub corner: Corner,
    /// Отступ от края по горизонтали; у центрального положения не действует.
    pub offset_x: i32,
    pub offset_y: i32,
}

impl Default for Anchor {
    fn default() -> Self {
        Self { enabled: false, monitor: 0, corner: Corner::TopLeft, offset_x: 10, offset_y: 10 }
    }
}

impl Anchor {
    pub fn sanitized(mut self) -> Anchor {
        self.offset_x = self.offset_x.clamp(0, MAX_OFFSET);
        self.offset_y = self.offset_y.clamp(0, MAX_OFFSET);
        self
    }
}

/// Монитор из настройки: `monitors` — пары «весь прямоугольник, основной ли» в порядке Windows.
/// Номер, которого больше нет (монитор отключили), — основной.
pub fn chosen_monitor(monitors: &[(Area, bool)], choice: usize) -> Option<Area> {
    let primary = || monitors.iter().find(|(_, primary)| *primary).or(monitors.first());
    let chosen = match choice {
        0 => primary(),
        index => monitors.get(index - 1).or_else(primary),
    };
    chosen.map(|(area, _)| *area)
}

/// Левый верхний угол HUD размером `size` в `area`. HUD не вылезает за монитор, даже если
/// отступ больше свободного места.
pub fn anchored_position(area: Area, size: (i32, i32), anchor: &Anchor) -> (i32, i32) {
    let (width, height) = size;
    let free_x = (area.width() - width).max(0);
    let free_y = (area.height() - height).max(0);
    let offset_x = anchor.offset_x.clamp(0, free_x);
    let offset_y = anchor.offset_y.clamp(0, free_y);
    let x = match anchor.corner {
        Corner::TopLeft | Corner::BottomLeft => offset_x,
        Corner::TopCenter | Corner::BottomCenter => free_x / 2,
        Corner::TopRight | Corner::BottomRight => free_x - offset_x,
    };
    let y = match anchor.corner {
        Corner::TopLeft | Corner::TopCenter | Corner::TopRight => offset_y,
        Corner::BottomLeft | Corner::BottomCenter | Corner::BottomRight => free_y - offset_y,
    };
    (area.left + x, area.top + y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Area = Area { left: -1920, top: 0, right: 0, bottom: 1080 };
    const MAIN: Area = Area { left: 0, top: 0, right: 2560, bottom: 1440 };

    fn anchor(corner: Corner, offset: i32) -> Anchor {
        Anchor { enabled: true, corner, offset_x: offset, offset_y: offset, ..Default::default() }
    }

    #[test]
    fn zero_means_primary_and_a_missing_monitor_falls_back_to_it() {
        let monitors = [(LEFT, false), (MAIN, true)];
        assert_eq!(chosen_monitor(&monitors, 0), Some(MAIN));
        assert_eq!(chosen_monitor(&monitors, 1), Some(LEFT));
        assert_eq!(chosen_monitor(&monitors, 2), Some(MAIN));
        assert_eq!(chosen_monitor(&monitors, 7), Some(MAIN), "монитор отключили");
        assert_eq!(chosen_monitor(&[], 0), None);
    }

    #[test]
    fn corners_keep_their_offsets() {
        let size = (272, 400);
        assert_eq!(anchored_position(MAIN, size, &anchor(Corner::TopLeft, 10)), (10, 10));
        assert_eq!(anchored_position(MAIN, size, &anchor(Corner::TopRight, 10)), (2278, 10));
        assert_eq!(anchored_position(MAIN, size, &anchor(Corner::BottomLeft, 10)), (10, 1030));
        assert_eq!(anchored_position(MAIN, size, &anchor(Corner::BottomRight, 0)), (2288, 1040));
        assert_eq!(anchored_position(MAIN, size, &anchor(Corner::TopCenter, 99)), (1144, 99));
    }

    #[test]
    fn a_monitor_left_of_the_primary_uses_its_own_coordinates() {
        let size = (272, 400);
        assert_eq!(anchored_position(LEFT, size, &anchor(Corner::TopRight, 10)), (-282, 10));
    }

    #[test]
    fn a_huge_offset_never_pushes_the_hud_off_screen() {
        let small = Area { left: 0, top: 0, right: 800, bottom: 600 };
        // По горизонтали отступ 500 помещается (свободно 528), по вертикали — нет (200).
        let (x, y) = anchored_position(small, (272, 400), &anchor(Corner::BottomRight, 500));
        assert_eq!((x, y), (28, 0));
        let (x, y) = anchored_position(small, (1000, 900), &anchor(Corner::TopLeft, 10));
        assert_eq!((x, y), (0, 0), "HUD больше монитора — прижат к углу");
    }

    #[test]
    fn offsets_are_clamped_on_load() {
        let fixed = Anchor { offset_x: -5, offset_y: 9_999, ..Default::default() }.sanitized();
        assert_eq!((fixed.offset_x, fixed.offset_y), (0, MAX_OFFSET));
    }
}

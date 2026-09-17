//! График frametime: что рисовать, без того, как рисовать (PLAN.md §6/P4).
//!
//! Отличия от `FrametimeGraph.kt` старого проекта:
//!
//! * **Окно по времени, а не по числу кадров.** 180 кадров при 240 FPS — это 0.75 с, при 60 FPS —
//!   3 с, и одинаковая картинка означала разное. Здесь окно всегда [`GRAPH_WINDOW_MS`].
//! * **Ось X — накопленная длительность кадров**, отсчитанная назад от последнего. Не время
//!   прихода: PresentMon отдаёт строки пачками, и по времени прихода кадры слипались бы.
//! * **Столбцы хранят пик.** Короткий провал не должен исчезнуть оттого, что в столбец попало
//!   ещё десять ровных кадров, — ради провалов график и смотрят.
//! * **Потолок по устойчивой оценке** (p95 × 1.25, не ниже [`FLOOR_CEILING_MS`]) с гистерезисом.
//!   Одиночный выброс в 500 мс больше не сплющивает ровные 8–16 мс в линию у нуля: он упирается
//!   в потолок, а UI рисует над ним маркер.

/// Сколько времени показывает график.
pub const GRAPH_WINDOW_MS: f64 = 10_000.0;
/// Столбцов на всё окно. UI растягивает их на свою ширину.
pub const GRAPH_COLUMNS: usize = 240;
/// Потолок никогда не ниже — 50 FPS целиком помещаются, и спокойная игра выглядит спокойно.
pub const FLOOR_CEILING_MS: f32 = 20.0;
/// Шаг, до которого округляется потолок: ровные подписи и меньше дёрганий.
const CEILING_STEP_MS: f32 = 5.0;
/// Потолок опускается, только если нужный стал ниже этой доли текущего.
const LOWER_BELOW: f32 = 0.7;

/// Готовая к рисованию картинка.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FrametimeGraph {
    /// Пик frametime в каждом столбце, старейший слева. `0.0` — в столбце кадров нет.
    pub columns: Vec<f32>,
    /// Верх шкалы в мс. Всё, что выше, UI срезает и помечает.
    pub ceiling_ms: f32,
}

impl Default for FrametimeGraph {
    fn default() -> Self {
        Self { columns: vec![0.0; GRAPH_COLUMNS], ceiling_ms: FLOOR_CEILING_MS }
    }
}

impl FrametimeGraph {
    pub fn is_empty(&self) -> bool {
        self.columns.iter().all(|value| *value <= 0.0)
    }

    /// Столбец выше потолка — рисуется срезанным, с маркером.
    pub fn is_clipped(&self, value: f32) -> bool {
        value > self.ceiling_ms
    }
}

/// Строит [`FrametimeGraph`] и помнит потолок между обновлениями.
#[derive(Debug, Clone, Default)]
pub struct GraphBuilder {
    session_id: Option<u64>,
    ceiling_ms: f32,
    scratch: Vec<f32>,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// `frametimes` — последние кадры, старейший первым. Лишнее за пределами окна отбрасывается.
    ///
    /// Смена `session_id` — другая игра или перезапуск захвата: потолок начинается заново, чтобы
    /// шкала прошлой игры не досталась новой.
    pub fn build(&mut self, session_id: u64, frametimes: &[f32]) -> FrametimeGraph {
        if self.session_id != Some(session_id) {
            self.session_id = Some(session_id);
            self.ceiling_ms = FLOOR_CEILING_MS;
        }

        let window = window_start(frametimes);
        let frames = &frametimes[window..];
        let columns = bucket(frames);

        let wanted = wanted_ceiling(frames, &mut self.scratch);
        self.ceiling_ms = next_ceiling(self.ceiling_ms.max(FLOOR_CEILING_MS), wanted);
        FrametimeGraph { columns, ceiling_ms: self.ceiling_ms }
    }
}

/// Сколько кадров с конца нужно, чтобы покрыть окно.
///
/// Возвращает индекс первого нужного кадра.
pub fn window_start(frametimes: &[f32]) -> usize {
    let mut covered = 0.0f64;
    for (index, frametime) in frametimes.iter().enumerate().rev() {
        if !frametime.is_finite() || *frametime <= 0.0 {
            continue;
        }
        covered += f64::from(*frametime);
        if covered >= GRAPH_WINDOW_MS {
            return index;
        }
    }
    0
}

fn bucket(frames: &[f32]) -> Vec<f32> {
    let mut columns = vec![0.0f32; GRAPH_COLUMNS];
    let column_ms = GRAPH_WINDOW_MS / GRAPH_COLUMNS as f64;
    // Кадр занимает отрезок [end - frametime, end] от правого края. Долгий кадр закрывает все
    // столбцы, через которые проходит, — иначе на 20 FPS между кадрами зияли бы дыры.
    let mut end = 0.0f64;
    for &frametime in frames.iter().rev() {
        if frametime <= 0.0 || !frametime.is_finite() {
            continue;
        }
        let start = end + f64::from(frametime);
        let first = (end / column_ms).floor() as usize;
        // Кадр, заканчивающийся ровно на границе столбца, в следующий не заходит.
        let last = ((start / column_ms).ceil() as usize).saturating_sub(1).max(first);
        for from_right in first..=last.min(GRAPH_COLUMNS - 1) {
            let column = &mut columns[GRAPH_COLUMNS - 1 - from_right];
            *column = column.max(frametime);
        }
        end = start;
        if end >= GRAPH_WINDOW_MS {
            break;
        }
    }
    columns
}

/// p95 × 1.25, округлённое вверх до шага. Пол здесь не применяется: порог спуска сравнивает
/// именно нужную высоту, иначе потолок застревал бы на шаг выше пола.
fn wanted_ceiling(frames: &[f32], scratch: &mut Vec<f32>) -> f32 {
    scratch.clear();
    scratch.extend(frames.iter().copied().filter(|value| value.is_finite() && *value > 0.0));
    if scratch.is_empty() {
        return 0.0;
    }
    let index = ((scratch.len() - 1) as f64 * 0.95).round() as usize;
    let (_, p95, _) = scratch.select_nth_unstable_by(index, f32::total_cmp);
    let raw = *p95 * 1.25;
    (raw / CEILING_STEP_MS).ceil() * CEILING_STEP_MS
}

/// Вверх — сразу, вниз — только при заметном запасе и по шагу за обновление.
///
/// Мгновенный подъём нужен, чтобы устойчиво тяжёлая сцена не жила вся под срезом. Медленный спуск
/// и порог — чтобы шкала не прыгала туда-сюда от сцены к сцене.
fn next_ceiling(current: f32, wanted: f32) -> f32 {
    if wanted > current {
        wanted
    } else if wanted < current * LOWER_BELOW {
        (current - CEILING_STEP_MS).max(wanted).max(FLOOR_CEILING_MS)
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady(frametime: f32, seconds: f64) -> Vec<f32> {
        vec![frametime; (seconds * 1_000.0 / f64::from(frametime)) as usize]
    }

    #[test]
    fn the_window_is_time_not_frame_count() {
        // 240 и 60 FPS: окно одинаковой длины, кадров в нём вчетверо разное число.
        let fast = steady(1_000.0 / 240.0, 30.0);
        let slow = steady(1_000.0 / 60.0, 30.0);
        let fast_frames = fast.len() - window_start(&fast);
        let slow_frames = slow.len() - window_start(&slow);
        assert!((2_390..=2_410).contains(&fast_frames), "{fast_frames}");
        assert!((595..=605).contains(&slow_frames), "{slow_frames}");
    }

    #[test]
    fn a_steady_game_fills_every_column() {
        let graph = GraphBuilder::new().build(1, &steady(16.7, 12.0));
        assert!(graph.columns.iter().all(|value| (*value - 16.7).abs() < 0.01));
    }

    #[test]
    fn slow_frames_leave_no_holes() {
        // 20 FPS: кадр длиннее столбца (≈41.7 мс).
        let graph = GraphBuilder::new().build(1, &steady(50.0, 12.0));
        assert!(graph.columns.iter().all(|value| *value == 50.0));
    }

    #[test]
    fn a_short_history_sits_at_the_right_edge() {
        let graph = GraphBuilder::new().build(1, &steady(10.0, 1.0));
        assert_eq!(graph.columns[GRAPH_COLUMNS - 1], 10.0);
        assert_eq!(graph.columns[0], 0.0);
        // Одна секунда из десяти — примерно десятая часть столбцов.
        let filled = graph.columns.iter().filter(|value| **value > 0.0).count();
        assert!((23..=25).contains(&filled), "{filled}");
    }

    #[test]
    fn a_short_spike_survives_bucketing() {
        let mut frames = steady(4.0, 11.0);
        let middle = frames.len() - 1_250; // ≈5 с до конца
        frames[middle] = 30.0;
        let graph = GraphBuilder::new().build(1, &frames);
        assert!(graph.columns.contains(&30.0));
    }

    #[test]
    fn a_single_huge_spike_does_not_flatten_the_scale() {
        let mut frames = steady(8.0, 11.0);
        let last = frames.len() - 100;
        frames[last] = 500.0;
        let graph = GraphBuilder::new().build(1, &frames);
        assert_eq!(graph.ceiling_ms, FLOOR_CEILING_MS);
        assert!(graph.is_clipped(500.0));
        assert!(!graph.is_clipped(8.0));
    }

    #[test]
    fn a_heavy_scene_raises_the_ceiling_at_once() {
        let mut builder = GraphBuilder::new();
        builder.build(1, &steady(8.0, 11.0));
        let graph = builder.build(1, &steady(33.0, 11.0));
        // 33 × 1.25 = 41.25 → 45.
        assert_eq!(graph.ceiling_ms, 45.0);
    }

    #[test]
    fn the_ceiling_comes_down_slowly_and_only_with_headroom() {
        let mut builder = GraphBuilder::new();
        assert_eq!(builder.build(1, &steady(33.0, 11.0)).ceiling_ms, 45.0);
        // 28 × 1.25 = 35 — больше 70 % от 45: шкала стоит.
        assert_eq!(builder.build(1, &steady(28.0, 11.0)).ceiling_ms, 45.0);
        // Лёгкая сцена: спуск по 5 мс за обновление до пола.
        let light = steady(8.0, 11.0);
        assert_eq!(builder.build(1, &light).ceiling_ms, 40.0);
        assert_eq!(builder.build(1, &light).ceiling_ms, 35.0);
        for _ in 0..10 {
            builder.build(1, &light);
        }
        assert_eq!(builder.build(1, &light).ceiling_ms, FLOOR_CEILING_MS);
    }

    #[test]
    fn a_new_session_starts_from_the_floor() {
        let mut builder = GraphBuilder::new();
        builder.build(1, &steady(33.0, 11.0));
        let graph = builder.build(2, &steady(8.0, 11.0));
        assert_eq!(graph.ceiling_ms, FLOOR_CEILING_MS);
    }

    #[test]
    fn garbage_values_are_ignored() {
        let frames = [f32::NAN, -1.0, 0.0, 10.0, f32::INFINITY];
        let graph = GraphBuilder::new().build(1, &frames);
        assert!(graph.columns.iter().all(|value| value.is_finite()));
        assert_eq!(graph.columns[GRAPH_COLUMNS - 1], 10.0);
    }

    #[test]
    fn no_frames_is_an_empty_graph() {
        let graph = GraphBuilder::new().build(1, &[]);
        assert!(graph.is_empty());
        assert_eq!(graph.ceiling_ms, FLOOR_CEILING_MS);
    }
}

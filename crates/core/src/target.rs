//! Политика выбора измеряемого процесса.
//!
//! Перенесено из `TargetTracker.kt` (PLAN.md §7.2). Правила, по порядку:
//!
//! 1. кандидата выдвигает окно в фокусе (оболочка и собственное окно не выдвигают никого);
//! 2. кандидат обязан продержаться впереди [`DEFAULT_DEBOUNCE_MS`], прежде чем сменит цель —
//!    иначе клик сквозь лаунчер перезапускает захват трижды;
//!    * **2а:** цель, у которой идут кадры, меняется только на кандидата, который сам рисует
//!      ([`SwitchGuard`]): Alt+Tab из игры в браузер или редактор не уводит захват с игры;
//! 3. пока в фокусе оболочка или наше окно — Alt+Tab, открытые настройки — цель **удерживается**:
//!    измеряется по-прежнему игра;
//! 4. цель, которой больше нет, сбрасывается немедленно; переиспользованный PID не наследует
//!    прошлый сеанс.
//!
//! Часы и окно в фокусе передаются в [`TargetTracker::resolve`] аргументами, а не берутся из
//! системы. Это и делает политику проверяемой без Windows — то, ради чего крейт существует.

use crate::Millis;
use crate::fps_state::{FpsReason, TargetProcess};

/// Сколько кандидат должен продержаться в фокусе, прежде чем станет целью.
pub const DEFAULT_DEBOUNCE_MS: Millis = 750;

/// Процессы оболочки, которые никогда не выдвигаются кандидатом.
///
/// Когда в фокусе они, это Alt+Tab, меню «Пуск» или диспетчер задач — пользователь отвлёкся, а
/// измеряется по-прежнему игра (правило 3 выше). `dwm.exe` здесь особенно важен: он презентит
/// через DXGI постоянно и чаще любого другого процесса, и выбор цели «по самому активному» выбрал
/// бы именно его (`spikes/etw-frames/COVERAGE.md` §4).
///
/// Перенесено из `ForegroundProcessDetector.kt`. Там список жил в слое Win32, здесь — в политике,
/// где его можно проверить без Windows.
const SHELL_PROCESSES: &[&str] = &[
    "explorer.exe",
    "dwm.exe",
    "searchhost.exe",
    "shellexperiencehost.exe",
    "applicationframehost.exe",
    "textinputhost.exe",
    "startmenuexperiencehost.exe",
    "lockapp.exe",
    "taskmgr.exe",
];

/// Оболочка ли это — то есть процесс, который целью не становится.
///
/// Сравнение без учёта регистра: Windows отдаёт имя образа так, как он лежит на диске.
pub fn is_shell_process(executable: &str) -> bool {
    let name = executable.rsplit(['\\', '/']).next().unwrap_or(executable).trim();
    SHELL_PROCESSES.iter().any(|shell| shell.eq_ignore_ascii_case(name))
}

/// Откуда берётся цель.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TargetMode {
    /// Из окна в фокусе.
    #[default]
    Auto,
    /// Задана пользователем.
    Manual,
}

/// Настройки выбора цели.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TargetSettings {
    pub mode: TargetMode,
    /// Имя exe для ручного режима.
    pub manual_process: Option<String>,
    /// PID для ручного режима: разрешает неоднозначность, когда копий процесса несколько.
    pub manual_pid: Option<u32>,
}

impl TargetSettings {
    pub fn auto() -> Self {
        Self { mode: TargetMode::Auto, ..Default::default() }
    }

    pub fn manual_by_name(name: impl Into<String>) -> Self {
        Self { mode: TargetMode::Manual, manual_process: Some(name.into()), manual_pid: None }
    }

    pub fn manual_by_pid(pid: u32) -> Self {
        Self { mode: TargetMode::Manual, manual_process: None, manual_pid: Some(pid) }
    }
}

/// Чем закончился один опрос.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TargetResolution {
    Resolved(TargetProcess),
    Unresolved { reason: FpsReason, detail: Option<String> },
}

impl TargetResolution {
    fn unresolved(reason: FpsReason) -> Self {
        TargetResolution::Unresolved { reason, detail: None }
    }

    fn unresolved_with(reason: FpsReason, detail: impl Into<String>) -> Self {
        TargetResolution::Unresolved { reason, detail: Some(detail.into()) }
    }

    pub fn target(&self) -> Option<&TargetProcess> {
        match self {
            TargetResolution::Resolved(target) => Some(target),
            TargetResolution::Unresolved { .. } => None,
        }
    }

    pub fn reason(&self) -> Option<FpsReason> {
        match self {
            TargetResolution::Resolved(_) => None,
            TargetResolution::Unresolved { reason, .. } => Some(*reason),
        }
    }
}

/// Доступ к списку процессов. Реализация живёт в `platform`, здесь — только интерфейс.
pub trait ProcessLookup {
    fn by_pid(&self, pid: u32) -> Option<TargetProcess>;
    fn find_by_name(&self, executable: &str) -> Vec<TargetProcess>;
    /// Жив ли **тот же запуск**, а не просто процесс с таким PID.
    fn is_same_run_alive(&self, target: &TargetProcess) -> bool;
}

/// Когда кандидат может сменить живую цель (правило 2а).
pub struct SwitchGuard<'a> {
    /// У текущей цели идут кадры. Нет кадров — терять нечего, смена разрешена всегда.
    pub target_has_frames: bool,
    /// Рисует ли кандидат сам. Спрашивается, только когда от ответа зависит решение.
    pub candidate_draws: &'a dyn Fn(&TargetProcess) -> bool,
}

impl SwitchGuard<'_> {
    /// Без ограничений: любой устоявшийся кандидат меняет цель.
    pub fn permissive() -> SwitchGuard<'static> {
        SwitchGuard { target_has_frames: false, candidate_draws: &|_| true }
    }

    fn allows(&self, current: Option<&TargetProcess>, candidate: &TargetProcess) -> bool {
        current.is_none() || !self.target_has_frames || (self.candidate_draws)(candidate)
    }
}

/// Держит текущую цель и кандидата на смену.
#[derive(Debug, Clone)]
pub struct TargetTracker {
    current: Option<TargetProcess>,
    candidate: Option<TargetProcess>,
    candidate_since_ms: Millis,
    debounce_ms: Millis,
}

impl Default for TargetTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TargetTracker {
    pub fn new() -> Self {
        Self::with_debounce(DEFAULT_DEBOUNCE_MS)
    }

    pub fn with_debounce(debounce_ms: Millis) -> Self {
        Self { current: None, candidate: None, candidate_since_ms: 0, debounce_ms }
    }

    /// Цель, действующая после последнего [`Self::resolve`].
    pub fn target(&self) -> Option<&TargetProcess> {
        self.current.as_ref()
    }

    /// Забывает цель — например, когда настройки сменили весь сеанс.
    pub fn forget(&mut self) {
        self.current = None;
        self.candidate = None;
    }

    /// Один опрос.
    ///
    /// `foreground` — процесс окна в фокусе; `None` означает «оболочка, наше окно или ничего»,
    /// то есть «кандидата нет», а вовсе не «цель пропала».
    pub fn resolve(
        &mut self,
        now_ms: Millis,
        foreground: Option<&TargetProcess>,
        settings: &TargetSettings,
        lookup: &dyn ProcessLookup,
    ) -> TargetResolution {
        self.resolve_guarded(now_ms, foreground, settings, lookup, &SwitchGuard::permissive())
    }

    /// [`Self::resolve`] с правилом 2а.
    pub fn resolve_guarded(
        &mut self,
        now_ms: Millis,
        foreground: Option<&TargetProcess>,
        settings: &TargetSettings,
        lookup: &dyn ProcessLookup,
        guard: &SwitchGuard<'_>,
    ) -> TargetResolution {
        match settings.mode {
            TargetMode::Auto => self.resolve_auto(now_ms, foreground, lookup, guard),
            TargetMode::Manual => self.resolve_manual(settings, lookup),
        }
    }

    fn resolve_auto(
        &mut self,
        now_ms: Millis,
        foreground: Option<&TargetProcess>,
        lookup: &dyn ProcessLookup,
        guard: &SwitchGuard<'_>,
    ) -> TargetResolution {
        if let Some(seen) = foreground {
            if seen.is_same_run_as_opt(self.current.as_ref()) {
                // В фокусе то, что и так измеряется, — кандидат больше не нужен.
                self.candidate = None;
            } else {
                if !seen.is_same_run_as_opt(self.candidate.as_ref()) {
                    self.candidate = Some(seen.clone());
                    self.candidate_since_ms = now_ms;
                }
                // Не пропущенный правилом 2а кандидат остаётся кандидатом: как только он начнёт
                // рисовать или цель потеряет кадры, смена случится без нового ожидания.
                if now_ms.saturating_sub(self.candidate_since_ms) >= self.debounce_ms
                    && guard.allows(self.current.as_ref(), seen)
                {
                    self.current = Some(seen.clone());
                    self.candidate = None;
                }
            }
        }

        let Some(held) = self.current.clone() else {
            return TargetResolution::unresolved(FpsReason::NoTarget);
        };

        if !lookup.is_same_run_alive(&held) {
            // Сбрасывается ТОЛЬКО цель. Кандидат, который уже копит свой debounce — например
            // та же игра, только что перезапущенная, — сохраняет накопленное время.
            self.current = None;
            return TargetResolution::unresolved_with(
                FpsReason::NoTarget,
                format!("{} завершился", held.executable),
            );
        }
        TargetResolution::Resolved(held)
    }

    fn resolve_manual(
        &mut self,
        settings: &TargetSettings,
        lookup: &dyn ProcessLookup,
    ) -> TargetResolution {
        // PID указан явно — он и решает, в том числе когда копий процесса несколько.
        if let Some(pid) = settings.manual_pid {
            return match lookup.by_pid(pid) {
                Some(found) => {
                    self.current = Some(found.clone());
                    TargetResolution::Resolved(found)
                }
                None => TargetResolution::unresolved_with(
                    FpsReason::TargetNotRunning,
                    format!("процесс с pid {pid} не запущен"),
                ),
            };
        }

        let name = match settings.manual_process.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name,
            _ => {
                return TargetResolution::unresolved_with(FpsReason::NoTarget, "процесс не выбран");
            }
        };

        let matches = lookup.find_by_name(name);
        if matches.is_empty() {
            self.forget();
            return TargetResolution::unresolved_with(
                FpsReason::TargetNotRunning,
                format!("{name} не запущен"),
            );
        }

        // Остаться на той копии, которая уже измеряется, лучше, чем переспрашивать на каждом
        // опросе: запуск второй копии не должен ронять идущий сеанс в неоднозначность.
        if let Some(current) = self.current.clone()
            && matches.iter().any(|found| found.is_same_run_as(&current))
        {
            return TargetResolution::Resolved(current);
        }

        if matches.len() > 1 {
            self.forget();
            return TargetResolution::unresolved_with(
                FpsReason::TargetAmbiguous,
                format!("процессов с именем {name}: {}", matches.len()),
            );
        }

        let only = matches.into_iter().next().expect("длина проверена выше");
        self.current = Some(only.clone());
        TargetResolution::Resolved(only)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Подставной список процессов: тесты управляют им напрямую.
    #[derive(Default)]
    struct FakeLookup {
        running: Vec<TargetProcess>,
    }

    impl ProcessLookup for FakeLookup {
        fn by_pid(&self, pid: u32) -> Option<TargetProcess> {
            self.running.iter().find(|p| p.pid == pid).cloned()
        }

        fn find_by_name(&self, executable: &str) -> Vec<TargetProcess> {
            self.running
                .iter()
                .filter(|p| p.executable.eq_ignore_ascii_case(executable))
                .cloned()
                .collect()
        }

        fn is_same_run_alive(&self, target: &TargetProcess) -> bool {
            self.running.iter().any(|p| p.is_same_run_as(target))
        }
    }

    /// Сцена замера: часы, фокус и список запущенного в одном месте.
    struct Scene {
        now: Millis,
        foreground: Option<TargetProcess>,
        lookup: FakeLookup,
        tracker: TargetTracker,
    }

    impl Scene {
        fn new(debounce_ms: Millis) -> Self {
            Self {
                now: 0,
                foreground: None,
                lookup: FakeLookup::default(),
                tracker: TargetTracker::with_debounce(debounce_ms),
            }
        }

        fn start(&mut self, pid: u32, name: &str, started_at_ms: Millis) -> TargetProcess {
            let process = TargetProcess::new(pid, name, Some(started_at_ms));
            self.lookup.running.push(process.clone());
            process
        }

        fn resolve(&mut self, settings: &TargetSettings) -> TargetResolution {
            self.tracker.resolve(self.now, self.foreground.as_ref(), settings, &self.lookup)
        }

        fn resolved(&mut self, settings: &TargetSettings) -> TargetProcess {
            match self.resolve(settings) {
                TargetResolution::Resolved(target) => target,
                other => panic!("ожидалась цель, получено {other:?}"),
            }
        }

        fn reason(&mut self, settings: &TargetSettings) -> FpsReason {
            match self.resolve(settings) {
                TargetResolution::Unresolved { reason, .. } => reason,
                other => panic!("ожидалось отсутствие цели, получено {other:?}"),
            }
        }

        /// Принимает то, что в фокусе: один опрос замечает кандидата, следующий — уже после
        /// debounce — делает его целью. Эти два шага и есть политика, а не деталь.
        fn adopt(&mut self) -> TargetProcess {
            let auto = TargetSettings::auto();
            self.resolve(&auto);
            self.now += 1_000;
            self.resolved(&auto)
        }
    }

    // --- авто ---------------------------------------------------------------------------

    #[test]
    fn nothing_in_the_foreground_means_no_target() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        assert_eq!(scene.reason(&TargetSettings::auto()), FpsReason::NoTarget);
    }

    /// Клик сквозь лаунчер не должен перезапускать захват на каждое окно.
    #[test]
    fn a_candidate_must_stay_in_front_before_it_becomes_the_target() {
        let mut scene = Scene::new(750);
        let auto = TargetSettings::auto();
        let game = scene.start(4242, "hl2.exe", 1_000);
        scene.foreground = Some(game.clone());

        assert_eq!(scene.reason(&auto), FpsReason::NoTarget, "ещё не устоялся");
        scene.now += 400;
        assert_eq!(scene.reason(&auto), FpsReason::NoTarget, "всё ещё не устоялся");
        scene.now += 400;
        assert_eq!(scene.resolved(&auto), game);
    }

    #[test]
    fn a_window_that_flashes_past_never_becomes_the_target() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let auto = TargetSettings::auto();
        let game = scene.start(4242, "hl2.exe", 1_000);
        let launcher = scene.start(99, "launcher.exe", 1_000);

        scene.foreground = Some(game.clone());
        assert_eq!(scene.adopt(), game);

        scene.foreground = Some(launcher);
        scene.now += 200;
        assert_eq!(scene.resolved(&auto), game, "измеряется по-прежнему игра");

        scene.foreground = Some(game.clone());
        scene.now += 200;
        assert_eq!(scene.resolved(&auto), game);
    }

    // --- правило 2а ----------------------------------------------------------------------

    fn guarded(
        scene: &mut Scene,
        target_has_frames: bool,
        draws: &dyn Fn(&TargetProcess) -> bool,
    ) -> TargetResolution {
        let guard = SwitchGuard { target_has_frames, candidate_draws: draws };
        let auto = TargetSettings::auto();
        scene.tracker.resolve_guarded(
            scene.now,
            scene.foreground.as_ref(),
            &auto,
            &scene.lookup,
            &guard,
        )
    }

    /// Alt+Tab из игры в редактор: игра рисует, редактор нет — цель остаётся.
    #[test]
    fn a_target_with_frames_is_kept_when_the_candidate_does_not_draw() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let game = scene.start(4242, "game.exe", 1_000);
        let editor = scene.start(77, "Code.exe", 1_000);
        scene.foreground = Some(game.clone());
        assert_eq!(scene.adopt(), game);

        scene.foreground = Some(editor);
        for _ in 0..10 {
            scene.now += 1_000;
            let resolution = guarded(&mut scene, true, &|_| false);
            assert_eq!(resolution.target(), Some(&game));
        }
    }

    /// Запущена вторая игра — она рисует, и цель переходит к ней.
    #[test]
    fn a_drawing_candidate_takes_over() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let game = scene.start(4242, "game.exe", 1_000);
        let other = scene.start(5000, "other.exe", 2_000);
        scene.foreground = Some(game.clone());
        assert_eq!(scene.adopt(), game);

        scene.foreground = Some(other.clone());
        guarded(&mut scene, true, &|_| true);
        scene.now += 1_000;
        assert_eq!(guarded(&mut scene, true, &|_| true).target(), Some(&other));
    }

    /// Кандидат начал рисовать позже — ждать заново не нужно.
    #[test]
    fn a_held_candidate_takes_over_as_soon_as_it_draws() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let game = scene.start(4242, "game.exe", 1_000);
        let loading = scene.start(5000, "other.exe", 2_000);
        scene.foreground = Some(game.clone());
        assert_eq!(scene.adopt(), game);

        scene.foreground = Some(loading.clone());
        guarded(&mut scene, true, &|_| false);
        scene.now += 5_000;
        assert_eq!(guarded(&mut scene, true, &|_| false).target(), Some(&game));
        scene.now += 250;
        assert_eq!(guarded(&mut scene, true, &|_| true).target(), Some(&loading));
    }

    /// У цели нет кадров — держаться не за что, работает обычное правило.
    #[test]
    fn a_target_without_frames_is_replaced_by_any_settled_candidate() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let editor = scene.start(77, "Code.exe", 1_000);
        let browser = scene.start(78, "chrome.exe", 1_000);
        scene.foreground = Some(editor.clone());
        assert_eq!(scene.adopt(), editor);

        scene.foreground = Some(browser.clone());
        let never_asked = |_: &TargetProcess| panic!("без кадров рисование не спрашивается");
        guarded(&mut scene, false, &never_asked);
        scene.now += 1_000;
        assert_eq!(guarded(&mut scene, false, &never_asked).target(), Some(&browser));
    }

    /// Первая цель выбирается без правила 2а.
    #[test]
    fn the_first_target_needs_no_drawing() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let editor = scene.start(77, "Code.exe", 1_000);
        scene.foreground = Some(editor.clone());
        guarded(&mut scene, true, &|_| false);
        scene.now += 1_000;
        assert_eq!(guarded(&mut scene, true, &|_| false).target(), Some(&editor));
    }

    /// Alt+Tab в проводник или открытие собственных настроек — не смена цели.
    #[test]
    fn the_target_is_held_while_the_foreground_is_a_shell_or_this_app() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let auto = TargetSettings::auto();
        let game = scene.start(4242, "hl2.exe", 1_000);

        scene.foreground = Some(game.clone());
        assert_eq!(scene.adopt(), game);

        // Для оболочек, композитора и наших собственных окон определитель отдаёт None.
        scene.foreground = None;
        scene.now += 5_000;
        assert_eq!(scene.resolved(&auto), game);
    }

    #[test]
    fn a_target_that_exits_is_dropped_even_while_alt_tabbed_away_from() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let auto = TargetSettings::auto();
        let game = scene.start(4242, "hl2.exe", 1_000);

        scene.foreground = Some(game);
        scene.adopt();

        scene.foreground = None;
        scene.lookup.running.clear();
        scene.now += 1_000;

        assert_eq!(scene.reason(&auto), FpsReason::NoTarget);
    }

    /// Windows быстро переиспользует PID: тот же номер — не та же игра.
    #[test]
    fn a_reused_pid_does_not_inherit_the_previous_session() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let auto = TargetSettings::auto();
        let first = scene.start(4242, "hl2.exe", 1_000);

        scene.foreground = Some(first.clone());
        assert_eq!(scene.adopt(), first);

        scene.lookup.running.clear();
        let second = scene.start(4242, "hl2.exe", 9_000);
        scene.foreground = Some(second);

        assert_eq!(scene.reason(&auto), FpsReason::NoTarget, "прошлого запуска больше нет");

        scene.now += 1_000;
        let current = scene.resolved(&auto);
        assert_eq!(current.started_at_ms, Some(9_000), "а новый запуск — новая цель");
    }

    /// Обратная сторона того же правила: мёртвая цель сбрасывается, но накопленный debounce
    /// кандидата не обнуляется (PLAN.md §6/P1).
    #[test]
    fn a_dying_target_does_not_reset_the_candidates_debounce() {
        let mut scene = Scene::new(750);
        let auto = TargetSettings::auto();
        let first = scene.start(4242, "hl2.exe", 1_000);

        scene.foreground = Some(first.clone());
        assert_eq!(scene.adopt(), first);

        // Игра перезапустилась: старый запуск исчез, новый уже в фокусе.
        scene.lookup.running.clear();
        let second = scene.start(5000, "hl2.exe", 9_000);
        scene.foreground = Some(second.clone());

        // Первый опрос: кандидат замечен, старая цель мертва.
        assert_eq!(scene.reason(&auto), FpsReason::NoTarget);

        // Ровно debounce спустя кандидат обязан стать целью — время ему засчитано с момента,
        // когда его заметили, а не с момента сброса старой цели.
        scene.now += 750;
        assert_eq!(scene.resolved(&auto), second);
    }

    #[test]
    fn switching_to_another_game_switches_the_capture() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let first = scene.start(4242, "hl2.exe", 1_000);
        let second = scene.start(777, "portal2.exe", 1_000);

        scene.foreground = Some(first.clone());
        assert_eq!(scene.adopt(), first);

        scene.foreground = Some(second.clone());
        assert_eq!(scene.adopt(), second);
    }

    #[test]
    fn forgetting_drops_the_current_target() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let auto = TargetSettings::auto();
        let game = scene.start(4242, "hl2.exe", 1_000);
        scene.foreground = Some(game);
        scene.adopt();

        scene.tracker.forget();
        scene.foreground = None;
        assert_eq!(scene.reason(&auto), FpsReason::NoTarget);
    }

    // --- оболочка -----------------------------------------------------------------------

    #[test]
    fn shell_processes_are_never_candidates() {
        for shell in ["explorer.exe", "dwm.exe", "taskmgr.exe", "LockApp.exe"] {
            assert!(is_shell_process(shell), "{shell} — оболочка");
        }
    }

    #[test]
    fn games_are_not_shell_processes() {
        for game in ["hl2.exe", "ROTTR.exe", "Gloomwood.exe", "dota2.exe", ""] {
            assert!(!is_shell_process(game), "«{game}» оболочкой не является");
        }
    }

    /// Windows отдаёт полный путь образа — сравнивать надо только имя файла.
    #[test]
    fn a_full_image_path_is_reduced_to_the_file_name() {
        assert!(is_shell_process(r"C:\Windows\explorer.exe"));
        assert!(is_shell_process("C:/Windows/System32/dwm.exe"));
        assert!(!is_shell_process(r"C:\Games\explorer.exe.bak"));
    }

    // --- ручной режим -------------------------------------------------------------------

    #[test]
    fn a_manual_name_resolves_to_the_one_process_carrying_it() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let game = scene.start(4242, "hl2.exe", 1_000);
        assert_eq!(scene.resolved(&TargetSettings::manual_by_name("HL2.EXE")), game);
    }

    #[test]
    fn a_manual_name_that_is_not_running_says_so() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        assert_eq!(
            scene.reason(&TargetSettings::manual_by_name("hl2.exe")),
            FpsReason::TargetNotRunning
        );
    }

    /// Две копии одного exe: по имени нельзя сказать, какая из них игра.
    #[test]
    fn two_processes_with_the_same_name_ask_the_user_to_choose() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        scene.start(4242, "hl2.exe", 1_000);
        scene.start(4243, "hl2.exe", 1_000);
        assert_eq!(
            scene.reason(&TargetSettings::manual_by_name("hl2.exe")),
            FpsReason::TargetAmbiguous
        );
    }

    #[test]
    fn a_manual_pid_settles_the_ambiguity() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        scene.start(4242, "hl2.exe", 1_000);
        let second = scene.start(4243, "hl2.exe", 1_000);
        assert_eq!(scene.resolved(&TargetSettings::manual_by_pid(4243)), second);
    }

    #[test]
    fn a_manual_pid_that_is_not_running_says_so() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        assert_eq!(scene.reason(&TargetSettings::manual_by_pid(4242)), FpsReason::TargetNotRunning);
    }

    /// Раз одна копия уже измеряется, она и остаётся измеряемой, а не становится неоднозначной.
    #[test]
    fn a_second_copy_starting_does_not_disturb_the_running_session() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        let manual = TargetSettings::manual_by_name("hl2.exe");
        let first = scene.start(4242, "hl2.exe", 1_000);
        assert_eq!(scene.resolved(&manual), first);

        scene.start(4243, "hl2.exe", 2_000);
        assert_eq!(scene.resolved(&manual), first);
    }

    #[test]
    fn an_empty_manual_name_is_not_a_target() {
        let mut scene = Scene::new(DEFAULT_DEBOUNCE_MS);
        assert_eq!(scene.reason(&TargetSettings::manual_by_name("   ")), FpsReason::NoTarget);
    }
}

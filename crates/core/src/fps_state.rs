//! Состояние источника кадров — одним значением.
//!
//! Перенесено из `FpsState.kt` (PLAN.md §7.3). Главное здесь не типы, а то, что доступность,
//! причина, цель и цифры лежат **вместе**. В старом проекте они какое-то время жили в четырёх
//! отдельных потоках, и это давало несогласованные сочетания: смена статуса без новых кадров
//! до UI просто не доходила. Одно значение не может противоречить самому себе.

use crate::Millis;
use crate::statistics::FrameStatistics;

/// Процесс, которому принадлежит поток кадров.
///
/// Имя — не идентичность. Windows быстро переиспользует PID, а две копии одного exe — это две
/// разные игры. Идентичностью служит пара «PID + время старта» (PLAN.md §6/P1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetProcess {
    pub pid: u32,
    pub executable: String,
    /// Момент старта процесса — **метка идентичности, а не время**.
    ///
    /// Значение только сравнивается на равенство и в единицах, которые выбрал слой платформы:
    /// на Windows это время создания процесса в миллисекундах от 1601 года. Сравнивать его с
    /// монотонными часами приложения нельзя.
    ///
    /// `None` означает «узнать не удалось» — тогда идентичность вырождается до PID, и это
    /// осознанная уступка, а не норма.
    pub started_at_ms: Option<Millis>,
}

impl TargetProcess {
    pub fn new(pid: u32, executable: impl Into<String>, started_at_ms: Option<Millis>) -> Self {
        Self { pid, executable: executable.into(), started_at_ms }
    }

    /// Один и тот же **запуск**, а не просто одинаковый номер.
    pub fn is_same_run_as(&self, other: &TargetProcess) -> bool {
        self.pid == other.pid && self.started_at_ms == other.started_at_ms
    }

    /// Удобная проверка против `Option`: цели может не быть вовсе.
    pub fn is_same_run_as_opt(&self, other: Option<&TargetProcess>) -> bool {
        other.is_some_and(|other| self.is_same_run_as(other))
    }
}

impl std::fmt::Display for TargetProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (pid {})", self.executable, self.pid)
    }
}

/// Насколько источник кадров реально работает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FpsAvailability {
    /// Ещё ничего не решено; только между созданием и первым опросом.
    #[default]
    Unknown,
    /// Источник исправен, но сейчас не измеряет: игры нет либо кадры ещё не пошли.
    ///
    /// Это честный ответ на «настроено, запущено, кадров нет» — состояние, которое прежняя
    /// реализация объявляла доступностью сразу по приходу заголовка CSV (PLAN.md §2.7).
    Waiting,
    /// Кадры идут.
    Available,
    /// Работать в текущей конфигурации не может.
    Unavailable,
    /// Сломался на ходу; показанные значения устарели.
    Error,
}

impl FpsAvailability {
    pub fn is_delivering(self) -> bool {
        self == FpsAvailability::Available
    }
}

/// Почему источник в текущем состоянии.
///
/// Коды, а не свободный текст: одна и та же ситуация обязана читаться одинаково, и строку лога
/// надо уметь сопоставить с состоянием. Именно это позволило в старом проекте локализовать
/// чужую поломку за две минуты вместо гадания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpsReason {
    Starting,
    Simulated,
    BackendNotConfigured,
    ExecutableMissing,
    NoTarget,
    TargetNotRunning,
    TargetAmbiguous,
    LaunchFailed,
    NotPermitted,
    SessionConflict,
    UnsupportedCsv,
    NoFrames,
    FramesStalled,
    SessionEnded,
    BackendFailed,
    /// PresentMon не отслеживает этот режим вывода и недосчитывает кадры.
    PresentModeUntracked,
}

impl FpsReason {
    /// Короткая строка для HUD. Текст живёт рядом с кодом, чтобы не разъезжаться.
    pub fn message(self) -> &'static str {
        match self {
            FpsReason::Starting => "запуск…",
            FpsReason::Simulated => "демонстрационные данные",
            FpsReason::BackendNotConfigured => "укажите путь к PresentMon.exe в настройках",
            FpsReason::ExecutableMissing => "PresentMon.exe не найден",
            FpsReason::NoTarget => "жду игру…",
            FpsReason::TargetNotRunning => "цель не запущена",
            FpsReason::TargetAmbiguous => "несколько процессов с таким именем",
            FpsReason::LaunchFailed => "не удалось запустить PresentMon",
            FpsReason::NotPermitted => "нужны права администратора",
            FpsReason::SessionConflict => "имя сессии захвата занято",
            FpsReason::UnsupportedCsv => "неподдерживаемый формат CSV",
            FpsReason::NoFrames => "кадров пока нет",
            FpsReason::FramesStalled => "кадры прекратились",
            FpsReason::SessionEnded => "захват завершён",
            FpsReason::BackendFailed => "PresentMon остановился",
            FpsReason::PresentModeUntracked => "PresentMon не видит кадры при выводе через GDI",
        }
    }
}

impl FpsReason {
    /// Причина сама говорит пользователю, что делать. Сырые слова источника («failed to start
    /// trace session: access denied») тут только мешают — они остаются в [`FpsState::detail`]
    /// для диагностики, а HUD показывает инструкцию.
    pub fn speaks_for_itself(self) -> bool {
        matches!(
            self,
            FpsReason::NotPermitted | FpsReason::ExecutableMissing | FpsReason::SessionConflict
        )
    }
}

impl std::fmt::Display for FpsReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Всё, что остальному приложению нужно знать о кадрах, — одним значением.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FpsState {
    pub availability: FpsAvailability,
    pub reason: Option<FpsReason>,
    /// Слова самого источника, когда они что-то добавляют: например строка, которую PresentMon
    /// напечатал в stderr.
    pub detail: Option<String>,
    pub target: Option<TargetProcess>,
    pub statistics: FrameStatistics,
    /// Растёт на каждый сеанс захвата, чтобы потребитель отличал историю одной игры от другой.
    pub session_id: u64,
    /// Когда принят последний кадр. `None`, пока сеанс не дал ни одного.
    pub last_frame_at_ms: Option<Millis>,
}

impl FpsState {
    pub const INITIAL: Self = Self {
        availability: FpsAvailability::Unknown,
        reason: None,
        detail: None,
        target: None,
        statistics: FrameStatistics::EMPTY,
        session_id: 0,
        last_frame_at_ms: None,
    };

    /// Одна короткая строка для HUD, либо ничего, если сказать нечего.
    ///
    /// Обычно слова источника важнее кода: они объясняют, что именно сломалось. Исключение —
    /// причины, которые сами являются инструкцией ([`FpsReason::speaks_for_itself`]).
    pub fn message(&self) -> Option<&str> {
        if let Some(reason) = self.reason.filter(|reason| reason.speaks_for_itself()) {
            return Some(reason.message());
        }
        self.detail.as_deref().or_else(|| self.reason.map(FpsReason::message))
    }

    /// Что печатается под строками FPS: кто измеряется и, когда это важно, почему цифр нет.
    /// Без этого замерший показатель и живой выглядят одинаково.
    pub fn summary(&self) -> Option<String> {
        let mut parts: Vec<&str> = Vec::with_capacity(2);
        if let Some(target) = &self.target {
            parts.push(&target.executable);
        }
        if let Some(message) = self.message() {
            parts.push(message);
        }
        if parts.is_empty() { None } else { Some(parts.join(" · ")) }
    }

    pub fn is_delivering(&self) -> bool {
        self.availability.is_delivering()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, started_at_ms: Option<Millis>) -> TargetProcess {
        TargetProcess::new(pid, "hl2.exe", started_at_ms)
    }

    #[test]
    fn identity_is_pid_and_start_time() {
        let first = process(4242, Some(1_000));
        let same = process(4242, Some(1_000));
        let reused_pid = process(4242, Some(9_000));
        let other_pid = process(777, Some(1_000));

        assert!(first.is_same_run_as(&same));
        assert!(!first.is_same_run_as(&reused_pid), "тот же PID — но другой запуск");
        assert!(!first.is_same_run_as(&other_pid));
    }

    /// Когда время старта узнать не удалось, идентичность вырождается до PID. Это допущение
    /// зафиксировано тестом, чтобы оно не стало незаметным.
    #[test]
    fn without_a_start_time_identity_degrades_to_the_pid() {
        let first = process(4242, None);
        let second = process(4242, None);
        assert!(first.is_same_run_as(&second));
        assert!(!first.is_same_run_as(&process(4242, Some(1_000))));
    }

    #[test]
    fn comparison_against_no_target_is_always_false() {
        assert!(!process(4242, Some(1_000)).is_same_run_as_opt(None));
    }

    #[test]
    fn detail_wins_over_the_reason_code() {
        let state = FpsState {
            reason: Some(FpsReason::BackendFailed),
            detail: Some("error: the parameter is incorrect".to_string()),
            ..FpsState::INITIAL
        };
        assert_eq!(state.message(), Some("error: the parameter is incorrect"));
    }

    /// Живой случай P4: без прав HUD показывал «failed to start trace session: access denied»
    /// вместо того, что делать.
    #[test]
    fn an_instruction_wins_over_raw_source_words() {
        let state = FpsState {
            reason: Some(FpsReason::NotPermitted),
            detail: Some("failed to start trace session: access denied.".to_string()),
            ..FpsState::INITIAL
        };
        assert_eq!(state.message(), Some("нужны права администратора"));
        assert!(state.detail.is_some(), "подробность сохраняется для диагностики");
    }

    #[test]
    fn the_summary_names_the_target_and_the_problem() {
        let state = FpsState {
            target: Some(process(4242, Some(1_000))),
            reason: Some(FpsReason::NoFrames),
            ..FpsState::INITIAL
        };
        assert_eq!(state.summary().as_deref(), Some("hl2.exe · кадров пока нет"));
    }

    #[test]
    fn an_empty_state_has_nothing_to_say() {
        assert_eq!(FpsState::INITIAL.summary(), None);
        assert_eq!(FpsState::INITIAL.message(), None);
        assert!(!FpsState::INITIAL.is_delivering());
    }

    /// Смысл §2.7: заголовок пришёл, кадров нет — это ожидание, а не доступность.
    #[test]
    fn waiting_is_not_delivering() {
        let state = FpsState {
            availability: FpsAvailability::Waiting,
            reason: Some(FpsReason::NoFrames),
            ..FpsState::INITIAL
        };
        assert!(!state.is_delivering());
        assert!(FpsAvailability::Available.is_delivering());
    }
}

//! Классификация ошибок PresentMon по stderr и коду возврата.
//!
//! Здесь живёт ловушка из PLAN.md §2.6, из-за которой прежняя реализация ставила ложный диагноз
//! «нет прав» на полностью рабочем захвате.
//!
//! PresentMon печатает при **любом** неэлевированном запуске предупреждение вида
//! «requires elevated privilege in order to query processes that are short-running…», включая
//! запуски, которые дальше работают нормально. Матчинг по всему stderr на слово «privilege»
//! поэтому ничего не стоит. Разбирать нужно **только** текст начиная с первой строки, которая
//! начинается на `error`.

use mh_core::FpsReason;

/// Сколько последних строк stderr держать.
///
/// Хвост, а не весь поток: поток ошибок может быть бесконечным, а полезны последние строки.
pub const MAX_ERROR_LINES: usize = 12;
/// До какой длины укорачивать строку для показа пользователю.
const MAX_DETAIL_CHARS: usize = 64;

/// Ограниченный хвост stderr.
#[derive(Debug, Default, Clone)]
pub struct BoundedTail {
    lines: std::collections::VecDeque<String>,
    capacity: usize,
    /// Сколько строк вытеснено. Ненулевое значение означает, что ошибок было больше показанного.
    dropped: usize,
}

impl BoundedTail {
    pub fn new(capacity: usize) -> Self {
        Self { lines: std::collections::VecDeque::new(), capacity: capacity.max(1), dropped: 0 }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line.into());
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }
}

/// Диагноз — общий тип для обоих источников кадров. `detail` здесь — первая строка `error:`
/// без префикса, укороченная для показа пользователю.
pub use crate::frames::source::Failure;

/// Разбирает хвост stderr.
///
/// Возвращает `None`, когда ни одной строки `error` нет: всё прочее — предупреждения, и поводом
/// объявить сеанс неудачным они не являются.
pub fn classify_stderr(tail: &BoundedTail) -> Option<Failure> {
    let lines: Vec<&str> = tail.lines().collect();
    // Всё до первой строки `error` — предупреждения, их читать нельзя.
    let first_error = lines.iter().position(|line| starts_with_error(line))?;
    let failure = &lines[first_error..];
    let text = failure.join(" ").to_lowercase();
    let detail = failure.first().map(|line| shorten(strip_error_prefix(line)));

    // «access denied» — то, что PresentMon 2.5.1 печатает без элевации, выходя с кодом 6.
    let reason = if text.contains("access denied")
        || text.contains("administrator")
        || text.contains("administrative")
        || text.contains("elevated privilege to start")
    {
        FpsReason::NotPermitted
    } else if text.contains("session")
        && (text.contains("already") || text.contains("in use") || text.contains("exists"))
    {
        FpsReason::SessionConflict
    } else {
        FpsReason::BackendFailed
    };

    Some(Failure { reason, detail })
}

/// Диагноз по коду возврата, когда stderr ничего не объяснил.
///
/// Коды проверены на 2.5.1 (PLAN.md §2.6): **6** — отказ в доступе, **1** — конфликт имени
/// сессии. Это запасной путь: текст надёжнее, потому что коды у разных сборок разъезжаются.
pub fn classify_exit_code(code: i32) -> Option<Failure> {
    match code {
        0 => None,
        6 => Some(Failure {
            reason: FpsReason::NotPermitted,
            detail: Some("PresentMon: отказано в доступе (код 6)".to_string()),
        }),
        other => Some(Failure {
            reason: FpsReason::BackendFailed,
            detail: Some(format!("PresentMon завершился с кодом {other}")),
        }),
    }
}

/// Итоговый диагноз: сначала текст, потом код возврата.
pub fn diagnose(tail: &BoundedTail, exit_code: Option<i32>) -> Option<Failure> {
    classify_stderr(tail).or_else(|| exit_code.and_then(classify_exit_code))
}

fn starts_with_error(line: &str) -> bool {
    line.trim_start().to_lowercase().starts_with("error")
}

fn strip_error_prefix(line: &str) -> &str {
    let trimmed = line.trim();
    // Снимаем ровно `error:` в любом регистре, а не любое слово, начинающееся на error.
    if trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("error:") {
        trimmed[6..].trim()
    } else {
        trimmed
    }
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= MAX_DETAIL_CHARS {
        return text.to_string();
    }
    let mut short: String = text.chars().take(MAX_DETAIL_CHARS - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ровно та строка, которую PresentMon 2.5.1 печатает при **любом** неэлевированном запуске,
    /// включая те, что дальше работают (PLAN.md §2.6).
    const HARMLESS_WARNING: &str = "warning: PresentMon requires elevated privilege in order to \
query processes that are short-running or belong to another user";

    /// А это настоящая ошибка отказа в доступе, с кодом возврата 6.
    const ACCESS_DENIED: &str = "error: failed to start trace session: access denied.";

    const SESSION_CONFLICT: &str = "warning: a trace session named \"PresentMon\" is already \
running and it will be stopped.";

    fn tail(lines: &[&str]) -> BoundedTail {
        let mut tail = BoundedTail::new(MAX_ERROR_LINES);
        for line in lines {
            tail.push(*line);
        }
        tail
    }

    /// Главный тест этого модуля: предупреждение про privilege не должно давать диагноз.
    #[test]
    fn the_privilege_warning_alone_is_not_a_failure() {
        assert_eq!(classify_stderr(&tail(&[HARMLESS_WARNING])), None);
    }

    /// И оно не должно перебивать разбор, когда настоящая ошибка идёт следом.
    #[test]
    fn a_warning_before_the_error_does_not_confuse_the_diagnosis() {
        let failure = classify_stderr(&tail(&[HARMLESS_WARNING, ACCESS_DENIED])).unwrap();
        assert_eq!(failure.reason, FpsReason::NotPermitted);
        assert_eq!(
            failure.detail.as_deref(),
            Some("failed to start trace session: access denied.")
        );
    }

    /// Ключевая проверка §2.6: текст ДО первой строки `error` не читается вовсе. Если бы
    /// разбор шёл по всему stderr, слово «session» из предупреждения дало бы ложный конфликт.
    #[test]
    fn only_text_from_the_first_error_line_counts() {
        let failure = classify_stderr(&tail(&[SESSION_CONFLICT, ACCESS_DENIED])).unwrap();
        assert_eq!(
            failure.reason,
            FpsReason::NotPermitted,
            "предупреждение про сессию стоит раньше ошибки и учитываться не должно"
        );
    }

    #[test]
    fn access_denied_means_not_permitted() {
        let failure = classify_stderr(&tail(&[ACCESS_DENIED])).unwrap();
        assert_eq!(failure.reason, FpsReason::NotPermitted);
    }

    #[test]
    fn a_session_name_clash_is_recognised() {
        let failure =
            classify_stderr(&tail(&["error: a trace session named \"X\" already exists"])).unwrap();
        assert_eq!(failure.reason, FpsReason::SessionConflict);
    }

    #[test]
    fn an_unrecognised_error_still_fails_the_session() {
        let failure = classify_stderr(&tail(&["error: something entirely new"])).unwrap();
        assert_eq!(failure.reason, FpsReason::BackendFailed);
        assert_eq!(failure.detail.as_deref(), Some("something entirely new"));
    }

    #[test]
    fn matching_ignores_case() {
        let failure = classify_stderr(&tail(&["ERROR: Access Denied."])).unwrap();
        assert_eq!(failure.reason, FpsReason::NotPermitted);
    }

    #[test]
    fn an_empty_stderr_yields_no_diagnosis() {
        assert_eq!(classify_stderr(&BoundedTail::new(4)), None);
    }

    #[test]
    fn a_long_detail_is_shortened_for_display() {
        let long = format!("error: {}", "x".repeat(200));
        let failure = classify_stderr(&tail(&[&long])).unwrap();
        let detail = failure.detail.unwrap();
        assert!(detail.chars().count() <= MAX_DETAIL_CHARS);
        assert!(detail.ends_with('…'));
    }

    // --- коды возврата ---

    #[test]
    fn exit_code_six_means_not_permitted() {
        assert_eq!(classify_exit_code(6).unwrap().reason, FpsReason::NotPermitted);
    }

    #[test]
    fn a_zero_exit_code_is_not_a_failure() {
        assert_eq!(classify_exit_code(0), None);
    }

    /// Текст надёжнее кода: коды у разных сборок разъезжаются, а сообщения — нет.
    #[test]
    fn stderr_text_wins_over_the_exit_code() {
        let failure = diagnose(&tail(&["error: a trace session already exists"]), Some(6)).unwrap();
        assert_eq!(failure.reason, FpsReason::SessionConflict);
    }

    #[test]
    fn the_exit_code_is_used_when_stderr_said_nothing() {
        let failure = diagnose(&tail(&[HARMLESS_WARNING]), Some(6)).unwrap();
        assert_eq!(failure.reason, FpsReason::NotPermitted);
    }

    #[test]
    fn a_clean_run_has_no_diagnosis_at_all() {
        assert_eq!(diagnose(&tail(&[HARMLESS_WARNING]), Some(0)), None);
        assert_eq!(diagnose(&BoundedTail::new(4), None), None);
    }

    // --- хвост ---

    #[test]
    fn the_tail_keeps_only_the_last_lines() {
        let mut tail = BoundedTail::new(3);
        for i in 0..10 {
            tail.push(format!("line {i}"));
        }
        assert_eq!(tail.lines().collect::<Vec<_>>(), vec!["line 7", "line 8", "line 9"]);
        assert_eq!(tail.dropped(), 7, "остальное вытеснено, и это видно");
    }

    /// Поток ошибок может быть бесконечным — память расти не должна.
    #[test]
    fn a_flood_of_errors_cannot_grow_memory() {
        let mut tail = BoundedTail::new(MAX_ERROR_LINES);
        for i in 0..100_000 {
            tail.push(format!("error: flood {i}"));
        }
        assert_eq!(tail.lines().count(), MAX_ERROR_LINES);
        // И диагноз по такому хвосту всё равно ставится.
        assert_eq!(classify_stderr(&tail).unwrap().reason, FpsReason::BackendFailed);
    }
}

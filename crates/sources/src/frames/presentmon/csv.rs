//! Разбор CSV PresentMon: схема по заголовку и чтение строк.
//!
//! Перенесено по смыслу из `PresentMonCsv.kt` (PLAN.md §7.4). Главное правило — **колонки
//! ищутся по именам заголовка, никогда по позициям**: 1.x, 2.x и варианты с `--v1_metrics`
//! располагают их по-разному, а реальный формат 2.5.1 вообще не содержит колонки `FrameTime`
//! (PLAN.md §2.5).

/// Ячейки одной строки CSV, с учётом кавычек и без единой аллокации.
///
/// Кавычки нужны потому, что имя процесса может содержать запятую. Внутренние пробелы
/// сохраняются: значение `Composed: Flip` — одна ячейка, а не две.
pub struct Cells<'a> {
    line: &'a str,
    position: usize,
    finished: bool,
}

pub fn cells(line: &str) -> Cells<'_> {
    Cells { line, position: 0, finished: false }
}

impl<'a> Iterator for Cells<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.finished {
            return None;
        }
        let bytes = self.line.as_bytes();
        let length = bytes.len();
        let mut cursor = self.position;
        while cursor < length && bytes[cursor] == b' ' {
            cursor += 1;
        }

        let (start, end);
        if cursor < length && bytes[cursor] == b'"' {
            cursor += 1;
            start = cursor;
            while cursor < length {
                if bytes[cursor] == b'"' {
                    // Удвоенная кавычка — экранированная, ячейка продолжается.
                    if cursor + 1 < length && bytes[cursor + 1] == b'"' {
                        cursor += 1;
                    } else {
                        break;
                    }
                }
                cursor += 1;
            }
            end = cursor;
            if cursor < length {
                cursor += 1;
            }
            while cursor < length && bytes[cursor] != b',' {
                cursor += 1;
            }
        } else {
            start = cursor;
            while cursor < length && bytes[cursor] != b',' {
                cursor += 1;
            }
            let mut trimmed = cursor;
            while trimmed > start && bytes[trimmed - 1] == b' ' {
                trimmed -= 1;
            }
            end = trimmed;
        }

        if cursor < length {
            self.position = cursor + 1;
        } else {
            // Завершающая запятая всё равно даёт последнюю — пустую — ячейку.
            self.finished = true;
        }
        Some(&self.line[start..end])
    }
}

/// Нормализованное имя колонки: нижний регистр, только буквы и цифры.
///
/// Так `Process_ID`, `ProcessID` и `process id` — одно и то же имя.
pub fn normalize_header_cell(cell: &str) -> String {
    cell.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Из какой колонки читается frametime и что это число означает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTimeMetric {
    /// Интервал между Present'ами приложения. Имя из PresentMon 1.x, и оно же в 2.5.1.
    MsBetweenPresents,
    /// То же измерение под именем из части сборок 2.x.
    FrameTime,
    /// Сколько кадр пробыл на экране. Это **другая метрика**: у отброшенного кадра её нет.
    /// Используется только по явному запросу и никогда как подстановка (PLAN.md §2.5).
    DisplayedTime,
}

impl FrameTimeMetric {
    pub fn header_names(self) -> &'static [&'static str] {
        match self {
            FrameTimeMetric::MsBetweenPresents => &["msbetweenpresents"],
            FrameTimeMetric::FrameTime => &["frametime"],
            FrameTimeMetric::DisplayedTime => &["msbetweendisplaychange", "displayedtime"],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FrameTimeMetric::MsBetweenPresents => "MsBetweenPresents",
            FrameTimeMetric::FrameTime => "FrameTime",
            FrameTimeMetric::DisplayedTime => "MsBetweenDisplayChange",
        }
    }

    /// Плавность в том виде, в каком её произвело приложение, а не в каком показал монитор.
    pub fn is_presented(self) -> bool {
        self != FrameTimeMetric::DisplayedTime
    }
}

/// Порядок предпочтения алиасов frametime (PLAN.md §2.5).
///
/// `msbetweenpresents` первым, потому что именно он есть в проверенной версии 2.5.1 и в 1.x;
/// `frametime` встречается лишь в части сборок 2.x. В Kotlin-проекте порядок был обратный — это
/// исправление, а не перенос.
const PRESENTED_PRIORITY: [FrameTimeMetric; 2] =
    [FrameTimeMetric::MsBetweenPresents, FrameTimeMetric::FrameTime];

const PROCESS_ID_NAMES: &[&str] = &["processid", "pid"];
const APPLICATION_NAMES: &[&str] = &["application", "processname", "process"];
const SWAP_CHAIN_NAMES: &[&str] = &["swapchainaddress", "swapchain"];
const SOURCE_TIME_NAMES: &[&str] = &["cpustarttime", "timeinseconds", "timeinms"];

/// Колонки одного поколения CSV, разобранные из заголовка **один раз**.
///
/// Дальше схема не перепроверяется: заголовок приходит ровно один, а сверять его на каждой
/// строке — тратить время на то, что не меняется.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub metric: FrameTimeMetric,
    pub frame_time_column: usize,
    pub process_id_column: Option<usize>,
    pub application_column: Option<usize>,
    pub swap_chain_column: Option<usize>,
    pub source_time_column: Option<usize>,
    pub column_count: usize,
}

impl Schema {
    /// `prefer_displayed` — читать интервал показа вместо интервала Present.
    ///
    /// Решение оставлено вызывающему, потому что это разные величины: ничего не переключается
    /// само и молча.
    pub fn parse(header: &str, prefer_displayed: bool) -> Option<Schema> {
        let names: Vec<String> =
            cells(header.trim_start_matches('\u{feff}')).map(normalize_header_cell).collect();
        if names.len() < 2 {
            return None;
        }

        let index_of = |candidates: &[&str]| -> Option<usize> {
            names.iter().position(|cell| candidates.contains(&cell.as_str()))
        };

        let metric = if prefer_displayed {
            [
                FrameTimeMetric::DisplayedTime,
                FrameTimeMetric::MsBetweenPresents,
                FrameTimeMetric::FrameTime,
            ]
            .into_iter()
            .find(|candidate| index_of(candidate.header_names()).is_some())?
        } else {
            PRESENTED_PRIORITY
                .into_iter()
                .find(|candidate| index_of(candidate.header_names()).is_some())?
        };

        Some(Schema {
            metric,
            frame_time_column: index_of(metric.header_names())?,
            process_id_column: index_of(PROCESS_ID_NAMES),
            application_column: index_of(APPLICATION_NAMES),
            swap_chain_column: index_of(SWAP_CHAIN_NAMES),
            source_time_column: index_of(SOURCE_TIME_NAMES),
            column_count: names.len(),
        })
    }

    /// Одна строка для лога: что именно нашлось в заголовке.
    pub fn describe(&self) -> String {
        let mut text = format!("{} @{}", self.metric.label(), self.frame_time_column);
        if let Some(column) = self.process_id_column {
            text.push_str(&format!(", pid @{column}"));
        }
        if let Some(column) = self.swap_chain_column {
            text.push_str(&format!(", swapchain @{column}"));
        }
        if let Some(column) = self.source_time_column {
            text.push_str(&format!(", время @{column}"));
        }
        text.push_str(&format!(", колонок {}", self.column_count));
        text
    }
}

/// Почему строка не стала кадром.
///
/// Считается, а не логируется: строка на каждый кадр — это сотни строк лога в секунду.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRejection {
    /// Колонки frametime в строке не оказалось.
    TooShort,
    /// В колонке frametime не число. `NA` — штатное значение и попадает сюда же.
    NotANumber,
    /// Число есть, но непригодно: ноль, отрицательное, бесконечность.
    OutOfRange,
    /// Строка чужого процесса.
    OtherProcess,
    /// Строка другой цепочки обмена.
    OtherSwapChain,
}

/// Один принятый кадр.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedFrame<'a> {
    pub frame_time_ms: f64,
    pub process_id: Option<u32>,
    /// Собственная метка времени PresentMon, в единицах выбранных флагов. Может отсутствовать.
    pub source_time: Option<f64>,
    pub swap_chain: Option<&'a str>,
}

/// Разбирает строку по схеме.
///
/// `process_id_filter` игнорируется, если в этом поколении CSV нет колонки PID: тогда
/// единственная фильтрация — та, что делает сам PresentMon по `--process_id`.
pub fn parse_row<'a>(
    schema: &Schema,
    line: &'a str,
    process_id_filter: Option<u32>,
) -> Result<ParsedFrame<'a>, RowRejection> {
    let mut frame_time_ms: Option<f64> = None;
    let mut process_id: Option<u32> = None;
    let mut source_time: Option<f64> = None;
    let mut swap_chain: Option<&str> = None;

    for (column, cell) in cells(line).enumerate() {
        if column == schema.frame_time_column {
            if cell.is_empty() {
                return Err(RowRejection::TooShort);
            }
            // `NA` — штатное значение PresentMon, а не поломка. Строку отбрасываем только
            // если NA стоит именно в колонке frametime (PLAN.md §2.5).
            let value: f64 = cell.parse().map_err(|_| RowRejection::NotANumber)?;
            if !value.is_finite() || value <= 0.0 {
                return Err(RowRejection::OutOfRange);
            }
            frame_time_ms = Some(value);
        } else if Some(column) == schema.process_id_column {
            process_id = cell.parse().ok();
            if let (Some(wanted), Some(seen)) = (process_id_filter, process_id)
                && wanted != seen
            {
                return Err(RowRejection::OtherProcess);
            }
        } else if Some(column) == schema.swap_chain_column {
            swap_chain = Some(cell);
        } else if Some(column) == schema.source_time_column {
            source_time = cell.parse().ok();
        }
    }

    let frame_time_ms = frame_time_ms.ok_or(RowRejection::TooShort)?;
    Ok(ParsedFrame { frame_time_ms, process_id, source_time, swap_chain })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Настоящий заголовок PresentMon 2.5.1, снятый с живого захвата (PLAN.md §2.5).
    const HEADER_2_5_1: &str = "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,\
PresentFlags,AllowsTearing,PresentMode,TimeInMs,MsBetweenSimulationStart,MsBetweenPresents,\
MsBetweenDisplayChange,MsInPresentAPI,MsRenderPresentLatency,MsUntilDisplayed,CPUStartTimeInMs,\
MsBetweenAppStart,MsCPUBusy,MsCPUWait,MsGPULatency,MsGPUTime,MsGPUBusy,MsGPUWait,MsAnimationError,\
AnimationTime,MsFlipDelay,MsAllInputToPhotonLatency,MsClickToPhotonLatency";

    /// Настоящая строка оттуда же. Обратите внимание на `NA` и на пробел внутри `Composed: Flip`.
    const ROW_2_5_1: &str = "Code.exe,18640,0x27A34FD9080,DXGI,0,0,0,Composed: Flip,199.8651,NA,\
119.21790000000000,116.62900000000000,0.11920000000000,0.37630000000000,5.0455,80.7585,119.2258,\
119.1066,0.1192,119.2792,0.2037,0.1955,0.0082,NA,80.7585,NA,NA,NA";

    // --- разбиение на ячейки ---

    #[test]
    fn a_plain_row_splits_on_commas() {
        assert_eq!(cells("a,b,c").collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }

    #[test]
    fn spaces_inside_a_value_are_kept() {
        // `Composed: Flip` — одна ячейка. Резать по пробелам нельзя.
        let row: Vec<&str> = cells("DXGI,Composed: Flip,199.8").collect();
        assert_eq!(row, vec!["DXGI", "Composed: Flip", "199.8"]);
    }

    #[test]
    fn quoted_cells_may_contain_commas() {
        let row: Vec<&str> = cells(r#""My Game, Deluxe.exe",4242,16.7"#).collect();
        assert_eq!(row, vec!["My Game, Deluxe.exe", "4242", "16.7"]);
    }

    #[test]
    fn empty_cells_are_preserved_including_a_trailing_one() {
        assert_eq!(cells("a,,c").collect::<Vec<_>>(), vec!["a", "", "c"]);
        assert_eq!(cells("a,b,").collect::<Vec<_>>(), vec!["a", "b", ""]);
    }

    #[test]
    fn surrounding_spaces_are_trimmed_but_not_inner_ones() {
        assert_eq!(cells(" a , b c ,d").collect::<Vec<_>>(), vec!["a", "b c", "d"]);
    }

    #[test]
    fn the_real_row_has_as_many_cells_as_the_real_header() {
        let header_cells = cells(HEADER_2_5_1).count();
        let row_cells = cells(ROW_2_5_1).count();
        assert_eq!(header_cells, 28, "в 2.5.1 двадцать восемь колонок");
        assert_eq!(row_cells, header_cells);
    }

    // --- схема ---

    #[test]
    fn header_names_are_normalized_before_matching() {
        assert_eq!(normalize_header_cell("Process_ID"), "processid");
        assert_eq!(normalize_header_cell("ProcessID"), "processid");
        assert_eq!(normalize_header_cell("  Ms Between Presents "), "msbetweenpresents");
    }

    /// Колонки `FrameTime` в 2.5.1 не существует — frametime лежит в `MsBetweenPresents`.
    #[test]
    fn the_real_header_resolves_to_msbetweenpresents() {
        let schema = Schema::parse(HEADER_2_5_1, false).expect("заголовок разобран");
        assert_eq!(schema.metric, FrameTimeMetric::MsBetweenPresents);
        assert_eq!(schema.frame_time_column, 10, "индекс 10, как в §2.5");
        assert_eq!(schema.process_id_column, Some(1));
        assert_eq!(schema.application_column, Some(0));
        assert_eq!(schema.swap_chain_column, Some(2));
        assert_eq!(schema.column_count, 28);
    }

    #[test]
    fn a_build_that_calls_it_frametime_also_resolves() {
        let schema = Schema::parse("Application,ProcessID,FrameTime", false).unwrap();
        assert_eq!(schema.metric, FrameTimeMetric::FrameTime);
        assert_eq!(schema.frame_time_column, 2);
    }

    /// Если есть оба имени, выигрывает то, что есть в проверенной версии.
    #[test]
    fn msbetweenpresents_wins_over_frametime() {
        let schema = Schema::parse("FrameTime,ProcessID,MsBetweenPresents", false).unwrap();
        assert_eq!(schema.metric, FrameTimeMetric::MsBetweenPresents);
        assert_eq!(schema.frame_time_column, 2);
    }

    /// Время показа — другая метрика, и подставляться вместо frametime она не должна.
    #[test]
    fn displayed_time_is_never_a_silent_substitute() {
        let header = "Application,ProcessID,MsBetweenDisplayChange";
        assert!(Schema::parse(header, false).is_none(), "молча брать её нельзя");

        let asked = Schema::parse(header, true).expect("по явному запросу — можно");
        assert_eq!(asked.metric, FrameTimeMetric::DisplayedTime);
        assert!(!asked.metric.is_presented());
    }

    #[test]
    fn a_header_without_any_frametime_column_is_unsupported() {
        assert!(Schema::parse("Application,ProcessID,PresentMode", false).is_none());
        assert!(Schema::parse("", false).is_none());
        assert!(Schema::parse("OnlyOneColumn", false).is_none());
    }

    #[test]
    fn a_utf8_bom_left_on_the_header_does_not_hide_the_first_column() {
        let schema =
            Schema::parse("\u{feff}Application,ProcessID,MsBetweenPresents", false).unwrap();
        assert_eq!(schema.application_column, Some(0));
    }

    // --- строки ---

    #[test]
    fn the_real_row_parses_into_a_frame() {
        let schema = Schema::parse(HEADER_2_5_1, false).unwrap();
        let frame = parse_row(&schema, ROW_2_5_1, None).expect("строка принята");
        assert!((frame.frame_time_ms - 119.2179).abs() < 1e-9);
        assert_eq!(frame.process_id, Some(18640));
        assert_eq!(frame.swap_chain, Some("0x27A34FD9080"));
    }

    /// `NA` в посторонней колонке — штатное значение, строку выбрасывать нельзя.
    #[test]
    fn na_outside_the_frametime_column_does_not_reject_the_row() {
        let schema = Schema::parse(HEADER_2_5_1, false).unwrap();
        assert!(ROW_2_5_1.contains(",NA,"), "в фикстуре есть NA");
        assert!(parse_row(&schema, ROW_2_5_1, None).is_ok());
    }

    /// А вот `NA` в самой колонке frametime — повод отбросить.
    #[test]
    fn na_in_the_frametime_column_rejects_the_row() {
        let schema = Schema::parse("ProcessID,MsBetweenPresents", false).unwrap();
        assert_eq!(parse_row(&schema, "4242,NA", None), Err(RowRejection::NotANumber));
    }

    #[test]
    fn non_positive_and_infinite_frametimes_are_out_of_range() {
        let schema = Schema::parse("ProcessID,MsBetweenPresents", false).unwrap();
        assert_eq!(parse_row(&schema, "1,0", None), Err(RowRejection::OutOfRange));
        assert_eq!(parse_row(&schema, "1,-5.0", None), Err(RowRejection::OutOfRange));
        assert_eq!(parse_row(&schema, "1,inf", None), Err(RowRejection::OutOfRange));
        assert_eq!(parse_row(&schema, "1,NaN", None), Err(RowRejection::OutOfRange));
    }

    #[test]
    fn a_short_row_is_rejected_not_misread() {
        let schema = Schema::parse(HEADER_2_5_1, false).unwrap();
        assert_eq!(parse_row(&schema, "Code.exe,18640", None), Err(RowRejection::TooShort));
    }

    #[test]
    fn rows_of_another_process_are_filtered_out() {
        let schema = Schema::parse("ProcessID,MsBetweenPresents", false).unwrap();
        assert!(parse_row(&schema, "4242,16.7", Some(4242)).is_ok());
        assert_eq!(parse_row(&schema, "777,16.7", Some(4242)), Err(RowRejection::OtherProcess));
    }

    /// Когда колонки PID нет, фильтровать нечем — и выдумывать фильтрацию нельзя.
    #[test]
    fn without_a_pid_column_the_filter_is_ignored() {
        let schema = Schema::parse("Application,MsBetweenPresents", false).unwrap();
        let frame = parse_row(&schema, "game.exe,16.7", Some(4242)).expect("строка принята");
        assert_eq!(frame.process_id, None);
    }

    #[test]
    fn a_quoted_application_name_does_not_shift_the_columns() {
        let schema = Schema::parse("Application,ProcessID,MsBetweenPresents", false).unwrap();
        let frame = parse_row(&schema, r#""Game, Deluxe.exe",4242,16.7"#, Some(4242)).unwrap();
        assert!((frame.frame_time_ms - 16.7).abs() < 1e-9);
        assert_eq!(frame.process_id, Some(4242));
    }
}

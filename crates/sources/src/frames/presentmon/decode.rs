//! Декодирование потока PresentMon в строки.
//!
//! PresentMon пишет stdout и stderr в **UTF-16LE с BOM** (PLAN.md §2.5). Но воспроизведение ETL
//! или другая сборка могут дать обычный UTF-8, поэтому кодировка определяется по BOM, а не
//! предполагается. BOM у UTF-8 тоже снимается — иначе он приклеится к первой ячейке заголовка,
//! и колонка `Application` перестанет находиться по имени.
//!
//! Декодер сделан «толкающим»: ему скармливают куски байтов как они пришли из трубы, он отдаёт
//! готовые строки. Так он проверяется на границах, которые в жизни и ломают наивную реализацию:
//! кусок, оборвавшийся посреди BOM, посреди пары UTF-16 или между `\r` и `\n`.

/// Кодировка потока, определённая по BOM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Максимальная длина строки, которую декодер готов копить.
///
/// Без предела повреждённый поток без единого перевода строки съест всю память. Строка длиннее
/// обрезается и отдаётся как есть: лучше испорченная строка, которую отбракует парсер, чем
/// растущий буфер.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

#[derive(Debug, Default)]
pub struct LineDecoder {
    encoding: Option<Encoding>,
    /// Байты, ещё не сложившиеся в строку. Для UTF-16 длина всегда чётная.
    pending: Vec<u8>,
    /// Сколько строк пришлось обрезать по [`MAX_LINE_BYTES`].
    truncated: usize,
}

impl LineDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Кодировка, если её уже удалось определить.
    pub fn encoding(&self) -> Option<Encoding> {
        self.encoding
    }

    pub fn truncated_lines(&self) -> usize {
        self.truncated
    }

    /// Добавляет очередной кусок и складывает готовые строки в `out`.
    ///
    /// Перевод строки в строку не попадает; `\r` перед ним снимается.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<String>) {
        self.pending.extend_from_slice(bytes);
        if self.encoding.is_none() {
            match sniff_bom(&self.pending) {
                Sniff::NeedMore => return,
                Sniff::Decided { encoding, bom_len } => {
                    self.encoding = Some(encoding);
                    self.pending.drain(..bom_len);
                }
            }
        }
        self.drain_lines(out);
    }

    /// Поток закончился: отдаёт остаток как последнюю строку, если он не пуст.
    pub fn finish(&mut self, out: &mut Vec<String>) {
        // Поток мог оборваться на неполном BOM — тогда это просто первые байты текста.
        if self.encoding.is_none() {
            self.encoding = Some(Encoding::Utf8);
        }
        self.drain_lines(out);
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            let line = self.decode(&rest);
            if !line.is_empty() {
                out.push(line);
            }
        }
    }

    fn drain_lines(&mut self, out: &mut Vec<String>) {
        let encoding = self.encoding.expect("кодировка определена выше");
        loop {
            match find_newline(&self.pending, encoding) {
                Some(at) => {
                    let unit = if encoding == Encoding::Utf8 { 1 } else { 2 };
                    let raw: Vec<u8> = self.pending.drain(..at + unit).collect();
                    let line = self.decode(&raw[..at]);
                    out.push(strip_carriage_return(line));
                }
                None => {
                    if self.pending.len() > MAX_LINE_BYTES {
                        // Предел достигнут, перевода строки нет. Отдаём что есть и продолжаем.
                        let unit = if encoding == Encoding::Utf8 { 1 } else { 2 };
                        let cut = MAX_LINE_BYTES - MAX_LINE_BYTES % unit;
                        let raw: Vec<u8> = self.pending.drain(..cut).collect();
                        out.push(self.decode(&raw));
                        self.truncated += 1;
                        continue;
                    }
                    return;
                }
            }
        }
    }

    fn decode(&self, bytes: &[u8]) -> String {
        match self.encoding.expect("кодировка определена выше") {
            // from_utf8_lossy и from_utf16_lossy, а не строгие варианты: оборванный поток не
            // повод потерять всю строку — испорченный символ отбракует парсер.
            Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            Encoding::Utf16Le => decode_utf16(bytes, u16::from_le_bytes),
            Encoding::Utf16Be => decode_utf16(bytes, u16::from_be_bytes),
        }
    }
}

fn decode_utf16(bytes: &[u8], to_unit: fn([u8; 2]) -> u16) -> String {
    // Незавершённая пара байтов в хвосте отбрасывается: `as_chunks` отдаёт её отдельно, и
    // склеивать половину символа не с чем.
    let (pairs, _tail) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|pair| to_unit(*pair)).collect();
    String::from_utf16_lossy(&units)
}

fn strip_carriage_return(mut line: String) -> String {
    if line.ends_with('\r') {
        line.pop();
    }
    line
}

enum Sniff {
    NeedMore,
    Decided { encoding: Encoding, bom_len: usize },
}

/// Определяет кодировку по началу потока.
///
/// Возвращает [`Sniff::NeedMore`] только когда начало **может** оказаться BOM, но байт ещё мало.
/// Ждать третий байт у `EF BB` безопасно: так начинается исключительно UTF-8 BOM, и поток,
/// которому пока нечего сказать, на этом не зависнет — `finish` разберёт остаток.
fn sniff_bom(bytes: &[u8]) -> Sniff {
    match bytes {
        [0xFF, 0xFE, ..] => Sniff::Decided { encoding: Encoding::Utf16Le, bom_len: 2 },
        [0xFE, 0xFF, ..] => Sniff::Decided { encoding: Encoding::Utf16Be, bom_len: 2 },
        [0xEF, 0xBB, 0xBF, ..] => Sniff::Decided { encoding: Encoding::Utf8, bom_len: 3 },
        [0xEF, 0xBB] => Sniff::NeedMore,
        [0xFF] | [0xFE] | [0xEF] => Sniff::NeedMore,
        [] => Sniff::NeedMore,
        _ => Sniff::Decided { encoding: Encoding::Utf8, bom_len: 0 },
    }
}

/// Смещение перевода строки в байтах, либо `None`.
fn find_newline(bytes: &[u8], encoding: Encoding) -> Option<usize> {
    match encoding {
        Encoding::Utf8 => bytes.iter().position(|&b| b == b'\n'),
        // Искать только по чётным смещениям обязательно: байт 0x0A может оказаться половиной
        // совсем другого символа.
        Encoding::Utf16Le => (0..bytes.len().saturating_sub(1))
            .step_by(2)
            .find(|&i| bytes[i] == 0x0A && bytes[i + 1] == 0x00),
        Encoding::Utf16Be => (0..bytes.len().saturating_sub(1))
            .step_by(2)
            .find(|&i| bytes[i] == 0x00 && bytes[i + 1] == 0x0A),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    fn decode_all(chunks: &[&[u8]]) -> (Vec<String>, LineDecoder) {
        let mut decoder = LineDecoder::new();
        let mut out = Vec::new();
        for chunk in chunks {
            decoder.push(chunk, &mut out);
        }
        decoder.finish(&mut out);
        (out, decoder)
    }

    #[test]
    fn plain_utf8_without_a_bom_is_the_default() {
        let (lines, decoder) = decode_all(&[b"first\nsecond\n"]);
        assert_eq!(lines, vec!["first", "second"]);
        assert_eq!(decoder.encoding(), Some(Encoding::Utf8));
    }

    #[test]
    fn a_utf8_bom_is_consumed_not_glued_to_the_first_cell() {
        let (lines, decoder) = decode_all(&[&[0xEF, 0xBB, 0xBF], b"Application,ProcessID\n"]);
        assert_eq!(lines, vec!["Application,ProcessID"]);
        assert_eq!(decoder.encoding(), Some(Encoding::Utf8));
    }

    /// Основной случай на Windows (PLAN.md §2.5).
    #[test]
    fn utf16le_with_a_bom_is_decoded() {
        let (lines, decoder) = decode_all(&[&utf16le("Application,ProcessID\nCode.exe,18640\n")]);
        assert_eq!(lines, vec!["Application,ProcessID", "Code.exe,18640"]);
        assert_eq!(decoder.encoding(), Some(Encoding::Utf16Le));
    }

    #[test]
    fn utf16be_is_decoded_too() {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in "hello\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        let (lines, decoder) = decode_all(&[&bytes]);
        assert_eq!(lines, vec!["hello"]);
        assert_eq!(decoder.encoding(), Some(Encoding::Utf16Be));
    }

    /// Труба отдаёт байты как захочет, а не строками. Разрыв посреди BOM — обычное дело.
    #[test]
    fn a_chunk_boundary_inside_the_bom_is_handled() {
        let full = utf16le("first\nsecond\n");
        let (lines, decoder) = decode_all(&[&full[..1], &full[1..]]);
        assert_eq!(lines, vec!["first", "second"]);
        assert_eq!(decoder.encoding(), Some(Encoding::Utf16Le));
    }

    #[test]
    fn a_chunk_boundary_inside_a_utf16_unit_is_handled() {
        let full = utf16le("Code.exe,18640\n");
        // Рвём на нечётном смещении — посреди пары байтов одного символа.
        let (lines, _) = decode_all(&[&full[..7], &full[7..]]);
        assert_eq!(lines, vec!["Code.exe,18640"]);
    }

    #[test]
    fn a_byte_at_a_time_still_yields_whole_lines() {
        let full = utf16le("one\ntwo\n");
        let chunks: Vec<&[u8]> = full.chunks(1).collect();
        let (lines, _) = decode_all(&chunks);
        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn crlf_leaves_no_carriage_return_behind() {
        let (lines, _) = decode_all(&[b"first\r\nsecond\r\n"]);
        assert_eq!(lines, vec!["first", "second"]);
    }

    #[test]
    fn a_split_between_cr_and_lf_is_not_a_line_break() {
        let (lines, _) = decode_all(&[b"first\r", b"\nsecond\n"]);
        assert_eq!(lines, vec!["first", "second"]);
    }

    #[test]
    fn a_trailing_line_without_a_newline_is_delivered_on_finish() {
        let (lines, _) = decode_all(&[b"complete\nincomplete"]);
        assert_eq!(lines, vec!["complete", "incomplete"]);
    }

    #[test]
    fn a_stream_that_ends_mid_bom_is_treated_as_text() {
        // Два байта, похожих на начало UTF-8 BOM, и больше ничего.
        let (lines, decoder) = decode_all(&[&[0xEF, 0xBB]]);
        assert_eq!(decoder.encoding(), Some(Encoding::Utf8));
        // Байты не образуют корректный UTF-8 — но и потерять их молча нельзя.
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn an_empty_stream_yields_nothing() {
        let (lines, _) = decode_all(&[]);
        assert!(lines.is_empty());
    }

    /// Поток без единого перевода строки не должен съесть память.
    #[test]
    fn an_endless_line_is_truncated_rather_than_buffered() {
        let mut decoder = LineDecoder::new();
        let mut out = Vec::new();
        let chunk = vec![b'x'; 8 * 1024];
        for _ in 0..20 {
            decoder.push(&chunk, &mut out);
        }
        assert!(!out.is_empty(), "обрезанная строка обязана быть отдана");
        assert!(decoder.truncated_lines() >= 1);
        assert!(out[0].len() <= MAX_LINE_BYTES);
    }

    #[test]
    fn empty_lines_are_preserved() {
        let (lines, _) = decode_all(&[b"first\n\nthird\n"]);
        assert_eq!(lines, vec!["first", "", "third"]);
    }
}

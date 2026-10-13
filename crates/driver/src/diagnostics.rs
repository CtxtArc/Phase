//! Rendering compiler diagnostics for humans and tools.
//!
//! Two formats:
//!
//! * `human` (default) -- rustc-style: the message, a `-->` location, the
//!   offending source line with the problem underlined, and each related
//!   note rendered the same way.
//! * `short` -- one `path:line:col: severity: message` line per diagnostic
//!   and note, the format editors and `grep` already understand.
//!
//! Spans come from the parser (statement or declaration level). When a
//! diagnostic names the entity it is about, the primary underline is
//! narrowed to that name's first whole-word occurrence inside the span.

use phase_ast::Span;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorFormat {
    Human,
    Short,
}

/// A diagnostic ready to render, independent of which compiler stage
/// produced it.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// `"error"` or `"warning"`.
    pub severity: &'static str,
    pub message: String,
    pub span: Option<Span>,
    pub subject: Option<String>,
    pub notes: Vec<(Span, String)>,
}

impl From<&phase_analysis::AnalysisError> for Diagnostic {
    fn from(e: &phase_analysis::AnalysisError) -> Self {
        Diagnostic {
            severity: match e.severity {
                phase_analysis::Severity::Error => "error",
                phase_analysis::Severity::Warning => "warning",
            },
            message: e.message.clone(),
            span: e.span,
            subject: e.subject.clone(),
            notes: e
                .notes
                .iter()
                .map(|n| (n.span, n.message.clone()))
                .collect(),
        }
    }
}

impl From<&phase_parser::ParseError> for Diagnostic {
    fn from(e: &phase_parser::ParseError) -> Self {
        Diagnostic {
            severity: "error",
            message: e.message.clone(),
            span: Some(e.span),
            subject: None,
            notes: Vec::new(),
        }
    }
}

/// ANSI styling, or nothing when color is off.
struct Style {
    on: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
    fn error(&self, t: &str) -> String {
        self.paint("1;31", t)
    }
    fn warning(&self, t: &str) -> String {
        self.paint("1;33", t)
    }
    /// The severity label and the primary underline share a color.
    fn severity(&self, sev: &str, t: &str) -> String {
        if sev == "warning" {
            self.warning(t)
        } else {
            self.error(t)
        }
    }
    fn note(&self, t: &str) -> String {
        self.paint("1;36", t)
    }
    fn bold(&self, t: &str) -> String {
        self.paint("1", t)
    }
    fn gutter(&self, t: &str) -> String {
        self.paint("1;34", t)
    }
}

/// Render `diag` (which occurred in `src`, read from `path`).
pub fn render(
    diag: &Diagnostic,
    path: &str,
    src: &str,
    format: ErrorFormat,
    color: bool,
) -> String {
    match format {
        ErrorFormat::Short => render_short(diag, path, src),
        ErrorFormat::Human => render_human(diag, path, src, &Style { on: color }),
    }
}

/// The column a location should be reported at: where the underline
/// starts (the subject's name, when it was found in the span).
fn reported_col(span: Span, src: &str, subject: Option<&str>) -> usize {
    match src.lines().nth(span.line.saturating_sub(1) as usize) {
        Some(line) => underline_range(span, line, subject).0,
        None => span.col as usize,
    }
}

fn render_short(diag: &Diagnostic, path: &str, src: &str) -> String {
    let mut out = String::new();
    match diag.span {
        Some(s) => writeln!(
            out,
            "{path}:{}:{}: {}: {}",
            s.line,
            reported_col(s, src, diag.subject.as_deref()),
            diag.severity,
            diag.message
        ),
        None => writeln!(out, "{path}: {}: {}", diag.severity, diag.message),
    }
    .unwrap();
    for (span, msg) in &diag.notes {
        writeln!(out, "{path}:{}:{}: note: {msg}", span.line, span.col).unwrap();
    }
    out
}

fn render_human(diag: &Diagnostic, path: &str, src: &str, st: &Style) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let width = std::iter::once(diag.span)
        .flatten()
        .chain(diag.notes.iter().map(|(s, _)| *s))
        .map(|s| s.line.to_string().len())
        .max()
        .unwrap_or(1);

    let mut out = String::new();
    writeln!(
        out,
        "{}{}",
        st.severity(diag.severity, diag.severity),
        st.bold(&format!(": {}", diag.message))
    )
    .unwrap();
    match diag.span {
        Some(span) => snippet(
            &mut out,
            path,
            &lines,
            span,
            diag.subject.as_deref(),
            '^',
            width,
            st,
            Some(diag.severity),
        ),
        None => writeln!(out, "{} {path}", st.gutter(&format!("{:>width$}-->", ""))).unwrap(),
    }
    for (span, msg) in &diag.notes {
        writeln!(out, "{}{}", st.note("note"), st.bold(&format!(": {msg}"))).unwrap();
        snippet(&mut out, path, &lines, *span, None, '-', width, st, None);
    }
    out
}

/// Columns `[start, end)` (1-based, in chars) to underline on `line_no`.
pub fn underline_range(span: Span, line: &str, subject: Option<&str>) -> (usize, usize) {
    let line_len = line.chars().count();
    let start = span.col.max(1) as usize;
    let end = if span.end_line == span.line && span.end_col as usize > start {
        span.end_col as usize
    } else {
        // Multi-line (or empty) span: underline to the end of its first line.
        line_len + 1
    };
    let end = end.min(line_len + 1).max(start + 1);

    if let Some(name) = subject {
        if let Some(col) = find_word(line, name, start, end) {
            return (col, col + name.chars().count());
        }
    }
    (start, end)
}

/// First whole-word occurrence of `word` in `line` starting within
/// columns `[from, to)`, as a 1-based column.
fn find_word(line: &str, word: &str, from: usize, to: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let w: Vec<char> = word.chars().collect();
    if w.is_empty() {
        return None;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let first = from.saturating_sub(1);
    let last = to.saturating_sub(1).min(chars.len());
    for i in first..last {
        if i + w.len() > chars.len() || chars[i..i + w.len()] != w[..] {
            continue;
        }
        let before_ok = i == 0 || !is_ident(chars[i - 1]);
        let after_ok = i + w.len() == chars.len() || !is_ident(chars[i + w.len()]);
        if before_ok && after_ok {
            return Some(i + 1);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn snippet(
    out: &mut String,
    path: &str,
    lines: &[&str],
    span: Span,
    subject: Option<&str>,
    mark: char,
    width: usize,
    st: &Style,
    // `Some(severity)` for the primary location, `None` for a note.
    primary: Option<&str>,
) {
    let blank = st.gutter(&format!("{:>width$} |", ""));
    let arrow = st.gutter(&format!("{:>width$}-->", ""));
    let Some(line) = lines.get(span.line.saturating_sub(1) as usize) else {
        writeln!(out, "{arrow} {path}:{}:{}", span.line, span.col).unwrap();
        return;
    };
    let (start, end) = underline_range(span, line, subject);
    writeln!(out, "{arrow} {path}:{}:{start}", span.line).unwrap();
    // Reuse the line's own tabs so the underline stays aligned.
    let pad: String = line
        .chars()
        .take(start - 1)
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let marks: String = std::iter::repeat_n(mark, end - start).collect();
    let marks = match primary {
        Some(sev) => st.severity(sev, &marks),
        None => st.note(&marks),
    };
    writeln!(out, "{blank}").unwrap();
    writeln!(
        out,
        "{} {line}",
        st.gutter(&format!("{:>width$} |", span.line))
    )
    .unwrap();
    writeln!(out, "{blank} {pad}{marks}").unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(line: u32, col: u32, end_line: u32, end_col: u32) -> Span {
        Span {
            line,
            col,
            end_line,
            end_col,
        }
    }

    const SRC: &str = "fn main() {\n    fir_filter(raw, filtered);\n}\n";

    fn diag(subject: Option<&str>) -> Diagnostic {
        Diagnostic {
            severity: "error",
            message: "entity 'raw' is in domain @DMA".into(),
            span: Some(span(2, 5, 2, 31)),
            subject: subject.map(str::to_string),
            notes: vec![(span(1, 1, 1, 12), "declared here".into())],
        }
    }

    #[test]
    fn human_format_underlines_the_subject() {
        let out = render(
            &diag(Some("raw")),
            "t.phase",
            SRC,
            ErrorFormat::Human,
            false,
        );
        let expected = "\
error: entity 'raw' is in domain @DMA
 --> t.phase:2:16
  |
2 |     fir_filter(raw, filtered);
  |                ^^^
note: declared here
 --> t.phase:1:1
  |
1 | fn main() {
  | -----------
";
        assert_eq!(out, expected);
    }

    #[test]
    fn without_a_subject_the_whole_statement_is_underlined() {
        let out = render(&diag(None), "t.phase", SRC, ErrorFormat::Human, false);
        assert!(
            out.contains("  |     ^^^^^^^^^^^^^^^^^^^^^^^^^^\n"),
            "{out}"
        );
    }

    #[test]
    fn subject_match_is_whole_word_only() {
        // `raw` must not match inside `raw_len`
        assert_eq!(find_word("x = raw_len(raw);", "raw", 1, 18), Some(13));
    }

    #[test]
    fn short_format_is_one_line_per_location() {
        let out = render(
            &diag(Some("raw")),
            "t.phase",
            SRC,
            ErrorFormat::Short,
            false,
        );
        assert_eq!(
            out,
            "t.phase:2:16: error: entity 'raw' is in domain @DMA\nt.phase:1:1: note: declared here\n"
        );
    }

    #[test]
    fn multi_line_span_underlines_to_end_of_first_line() {
        let src = "if x {\n  y();\n}\n";
        let (s, e) = underline_range(span(1, 1, 3, 2), "if x {", None);
        assert_eq!((s, e), (1, 7));
        let _ = src;
    }

    #[test]
    fn color_wraps_in_ansi_codes() {
        let out = render(&diag(Some("raw")), "t.phase", SRC, ErrorFormat::Human, true);
        assert!(out.starts_with("\x1b[1;31merror\x1b[0m"));
    }
}

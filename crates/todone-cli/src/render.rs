//! Rendering context lines with ANSI colors.
//!
//! The renderer composes two layers per line: syntax-highlight spans
//! (foreground colors) and the selection overlay (background color).

use std::ops::Range;

use crate::context::Context;
use crate::highlight::HighlightSpan;

/// ANSI escape for a truecolor foreground.
fn fg(color: (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{};{};{}m", color.0, color.1, color.2)
}

/// ANSI escape for a truecolor background.
fn bg(color: (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{};{};{}m", color.0, color.1, color.2)
}

const RESET: &str = "\x1b[0m";

/// The background color used to mark the selected comment range.
pub const SELECTION_BG: (u8, u8, u8) = (38, 46, 58);

/// The gutter color for line numbers.
pub const GUTTER_FG: (u8, u8, u8) = (90, 100, 110);

/// Whether a byte offset lies inside a range.
fn in_range(offset: usize, range: &Range<usize>) -> bool {
    offset >= range.start && offset < range.end
}

/// The resolved style at one character position: syntax color plus whether
/// the selection covers it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CharStyle {
    fg: Option<(u8, u8, u8)>,
    bold: bool,
    selected: bool,
}

/// Renders one context line.
///
/// In colored mode the line is prefixed by a gutter with the line number,
/// syntax colors are applied from `spans`, and the `selection` range (file
/// bytes) gets a background highlight. In plain mode the output contains no
/// ANSI escapes and the selected marker is `>`.
///
/// # Examples
///
/// ```
/// use todone_cli::render::render_context_line;
///
/// let plain = render_context_line("  // TODO: x", 5, 0, &[], &(0..13), false, 3);
/// assert_eq!(plain, "    5 │   // TODO: x");
/// ```
pub fn render_context_line(
    line_text: &str,
    line_number: usize,
    line_byte_offset: usize,
    spans: &[HighlightSpan],
    selection: &Range<usize>,
    color: bool,
    gutter_width: usize,
) -> String {
    let marker = if selection_overlaps_line(line_byte_offset, line_text.len(), selection) {
        '>'
    } else {
        ' '
    };

    let body = if color {
        render_colored(line_text, line_byte_offset, spans, selection)
    } else {
        line_text.to_string()
    };

    let gutter = if color {
        format!(
            " {marker} {}{:>width$}{} │ ",
            fg(GUTTER_FG),
            line_number,
            RESET,
            width = gutter_width
        )
    } else {
        format!(" {marker} {line_number:>width$} │ ", width = gutter_width)
    };

    format!("{gutter}{body}{}", if color { RESET } else { "" })
}

/// Whether the selection covers any byte of the line at `line_byte_offset`.
fn selection_overlaps_line(
    line_byte_offset: usize,
    line_len: usize,
    selection: &Range<usize>,
) -> bool {
    let line_end = line_byte_offset + line_len;
    selection.start < line_end && selection.end > line_byte_offset
}

/// Renders the line body with per-character style resolution.
fn render_colored(
    line_text: &str,
    line_byte_offset: usize,
    spans: &[HighlightSpan],
    selection: &Range<usize>,
) -> String {
    let mut styles = Vec::new();
    for (i, _) in line_text.char_indices() {
        let offset = line_byte_offset + i;
        let span = spans.iter().rev().find(|s| in_range(offset, &s.range));
        styles.push(CharStyle {
            fg: span.and_then(|s| s.fg),
            bold: span.is_some_and(|s| s.bold),
            selected: in_range(offset, selection),
        });
    }

    let mut out = String::new();
    let mut current = CharStyle::default();
    let mut first = true;
    for (i, ch) in line_text.char_indices() {
        let style = styles.get(i).copied().unwrap_or_default();
        if first || style != current {
            out.push_str(RESET);
            if style.selected {
                out.push_str(&bg(SELECTION_BG));
            }
            if let Some(fg_color) = style.fg {
                out.push_str(&fg(fg_color));
            }
            if style.bold {
                out.push_str("\x1b[1m");
            }
            current = style;
            first = false;
        }
        out.push(ch);
    }
    out
}

/// Renders a full context block (gutter + lines).
///
/// # Examples
///
/// ```
/// use todone_core::model::{Comment, CommentRun, Finding, Selection};
/// use todone_cli::context::extract_context;
/// use todone_cli::render::render_context;
///
/// let source = "fn main() {\n    // TODO: fix\n}\n";
/// let run = CommentRun {
///     comments: vec![Comment {
///         path: "a.rs".into(),
///         line: 2,
///         end_line: 2,
///         column: 0,
///         byte_range: 11..23,
///         text: "// TODO: fix".into(),
///         language: "rust".into(),
///     }],
/// };
/// let finding = Finding {
///     run,
///     category: "TODO".into(),
///     primary: 0,
///     selection: Selection::full(1),
/// };
/// let context = extract_context(source, &finding, 1, 1);
/// let rendered = render_context(&context, &[], false);
/// assert!(rendered.contains("│    // TODO: fix"));
/// assert!(rendered.contains(" > 2 │ "));
/// ```
pub fn render_context(context: &Context, spans: &[HighlightSpan], color: bool) -> String {
    let width = context
        .lines
        .iter()
        .map(|l| l.line.to_string().len())
        .max()
        .unwrap_or(1);
    let mut out = String::new();
    for line in &context.lines {
        out.push_str(&render_context_line(
            &line.text,
            line.line,
            line.byte_range.start,
            spans,
            &context.selection,
            color,
            width,
        ));
        out.push('\n');
    }
    out
}

/// Renders one line of a compact single-line summary (no gutter).
pub fn render_finding_header(path: &str, line: usize, category: &str, color: bool) -> String {
    if color {
        format!(
            "\x1b[1m{path}:{line}:\x1b[0m {}{}{}",
            fg((220, 160, 70)),
            category,
            RESET
        )
    } else {
        format!("{path}:{line}: {category}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(
        text: &str,
        number: usize,
        offset: usize,
        kind: crate::context::LineKind,
    ) -> crate::context::ContextLine {
        crate::context::ContextLine {
            line: number,
            text: text.into(),
            kind,
            byte_range: offset..offset + text.len(),
        }
    }

    #[test]
    fn plain_output_has_no_ansi() {
        let text = "  // TODO: x";
        let out = render_context_line(text, 5, 0, &[], &(0..0), false, 3);
        assert!(out.ends_with("│   // TODO: x"));
        assert!(out.starts_with(' '));
        assert!(!out.contains('\x1b'));
        assert!(out.contains("5 │"));
    }

    #[test]
    fn selected_line_gets_marker() {
        let out = render_context_line("// TODO: x", 5, 0, &[], &(0..11), false, 3);
        assert!(out.starts_with(" > "));
        let out = render_context_line("code", 5, 0, &[], &(100..200), false, 3);
        assert!(out.starts_with("   "));
    }

    #[test]
    fn colored_output_contains_ansi() {
        let out = render_context_line("// TODO: x", 5, 0, &[], &(0..11), true, 3);
        assert!(out.contains('\x1b'));
        assert!(out.contains(RESET));
    }

    #[test]
    fn selection_background_is_applied() {
        let out = render_context_line("let x = 1;", 1, 0, &[], &(0..11), true, 1);
        assert!(out.contains(&bg(SELECTION_BG)));
    }

    #[test]
    fn syntax_spans_color_the_body() {
        let spans = vec![HighlightSpan {
            range: 0..2,
            fg: Some((255, 0, 0)),
            bold: false,
            italic: false,
        }];
        let out = render_context_line("let", 1, 0, &spans, &(0..0), true, 1);
        assert!(out.contains(&fg((255, 0, 0))));
    }

    #[test]
    fn render_context_builds_a_block() {
        let ctx = crate::context::Context {
            lines: vec![
                line("a", 1, 0, crate::context::LineKind::Before),
                line("// TODO", 2, 2, crate::context::LineKind::Selected),
                line("b", 3, 10, crate::context::LineKind::After),
            ],
            selection: 2..9,
        };
        let out = render_context(&ctx, &[], false);
        assert!(out.contains("│ a"));
        assert!(out.contains(" > 2 │ // TODO"));
    }

    #[test]
    fn header_renders() {
        let plain = render_finding_header("src/lib.rs", 5, "TODO", false);
        assert_eq!(plain, "src/lib.rs:5: TODO");
        assert!(render_finding_header("src/lib.rs", 5, "TODO", true).contains('\x1b'));
    }
}

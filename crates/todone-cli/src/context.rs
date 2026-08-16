//! Context extraction: the `-A`/`-B` window around a finding, plus the
//! byte ranges the renderer needs to apply syntax highlighting.

use std::ops::Range;

use todone_core::model::Finding;

/// What role a context line plays relative to the finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    /// Before the selection.
    Before,
    /// Inside the selected comment range.
    Selected,
    /// After the selection.
    After,
}

/// One line of the context window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ContextLine {
    /// 1-based line number in the file.
    pub line: usize,
    /// The line text, without its terminator.
    pub text: String,
    /// Role of the line.
    pub kind: LineKind,
    /// Byte range of the line within the file (excluding the terminator).
    pub byte_range: Range<usize>,
}

/// The context window around one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// The lines, in file order.
    pub lines: Vec<ContextLine>,
    /// Byte range of the selected comments within the file.
    pub selection: Range<usize>,
}

/// Extracts the context window for `finding` from `source`.
///
/// `before`/`after` are the numbers of context lines; they clamp at the
/// file's edges. Lines covered by the selection are marked [`LineKind::Selected`].
///
/// # Examples
///
/// ```
/// use todone_core::model::{Comment, CommentRun, Finding, Selection};
/// use todone_cli::context::{Context, LineKind, extract_context};
///
/// let source = "line one\n// TODO: x\nline three\n";
/// let run = CommentRun {
///     comments: vec![Comment {
///         path: "a.rs".into(),
///         line: 2,
///         end_line: 2,
///         column: 0,
///         byte_range: 9..19,
///         text: "// TODO: x".into(),
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
/// assert_eq!(context.lines.len(), 3);
/// assert_eq!(context.lines[1].kind, LineKind::Selected);
/// ```
pub fn extract_context(source: &str, finding: &Finding, before: usize, after: usize) -> Context {
    let run = &finding.run;
    let first = run.comments[finding.selection.start].line;
    let last = run.comments[finding.selection.end].end_line;

    let total = source.lines().count().max(1);
    let start = first.saturating_sub(before);
    let end = (last + after).min(total);

    let mut offset = 0;
    let mut lines = Vec::new();
    for (index, raw) in source.lines().enumerate() {
        let line = index + 1;
        let range = offset..offset + raw.len();
        offset += raw.len() + 1;
        if line < start || line > end {
            continue;
        }
        let kind = if line >= first && line <= last {
            LineKind::Selected
        } else if line < first {
            LineKind::Before
        } else {
            LineKind::After
        };
        lines.push(ContextLine {
            line,
            text: raw.to_string(),
            kind,
            byte_range: range,
        });
    }

    Context {
        lines,
        selection: finding.selected_range(),
    }
}

/// The context lines shown by default.
#[cfg(test)]
mod tests {
    use super::*;
    use todone_core::model::{Comment, CommentRun, Finding, Selection};

    fn finding(line: usize, end_line: usize) -> Finding {
        Finding {
            run: CommentRun {
                comments: vec![Comment {
                    path: "a.rs".into(),
                    line,
                    end_line,
                    column: 0,
                    byte_range: 0..10,
                    text: "// TODO: x".into(),
                    language: "rust".into(),
                }],
            },
            category: "TODO".into(),
            primary: 0,
            selection: Selection::full(1),
        }
    }

    #[test]
    fn window_clamps_at_edges() {
        let source = "1\n2\n3\n4\n5\n";
        let finding = finding(3, 3);
        let context = extract_context(source, &finding, 1, 1);
        let nums: Vec<_> = context.lines.iter().map(|l| l.line).collect();
        assert_eq!(nums, vec![2, 3, 4]);
        assert_eq!(context.lines[1].kind, LineKind::Selected);
        assert_eq!(context.lines[0].kind, LineKind::Before);
        assert_eq!(context.lines[2].kind, LineKind::After);
    }

    #[test]
    fn window_at_file_start() {
        let source = "1\n2\n3\n";
        let finding = finding(1, 1);
        let context = extract_context(source, &finding, 5, 5);
        let nums: Vec<_> = context.lines.iter().map(|l| l.line).collect();
        assert_eq!(nums, vec![1, 2, 3]);
    }

    #[test]
    fn multi_line_selection_marks_all_lines() {
        let source = "1\n2\n3\n4\n";
        let finding = finding(2, 3);
        let context = extract_context(source, &finding, 0, 0);
        assert!(context.lines.iter().all(|l| l.kind == LineKind::Selected));
    }

    #[test]
    fn line_ranges_are_byte_accurate() {
        let source = "ab\ncde\nfg\n";
        let finding = finding(2, 2);
        let context = extract_context(source, &finding, 1, 1);
        assert_eq!(context.lines[0].byte_range, 0..2);
        assert_eq!(context.lines[1].byte_range, 3..6);
        assert_eq!(context.lines[2].byte_range, 7..9);
    }

    #[test]
    fn selection_range_passes_through() {
        let source = "ab\n// TODO: x\n";
        let finding = finding(2, 2);
        let context = extract_context(source, &finding, 0, 0);
        assert_eq!(context.selection, 0..10);
    }
}

//! The domain model: comments, comment runs, findings, and selections.
//!
//! All paths in this module are **repository-relative** (as produced by the
//! scanner); byte ranges are offsets into the file the comment lives in.

use std::ops::Range;
use std::path::PathBuf;

use serde::Serialize;

/// A single comment node found by the tree-sitter scanner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Comment {
    /// Path relative to the repository root.
    pub path: PathBuf,
    /// 1-based line of the comment's first character.
    pub line: usize,
    /// 1-based line of the comment's last character.
    pub end_line: usize,
    /// 0-based byte column of the comment's first character.
    pub column: usize,
    /// Byte range of the comment node within the file.
    pub byte_range: Range<usize>,
    /// Full comment text, including its marker (e.g. `// TODO: x`).
    pub text: String,
    /// Language id the comment was parsed with (see [`crate::language`]).
    pub language: String,
}

/// A maximal run of adjacent comment nodes: comments on the same line or on
/// immediately consecutive lines are considered attached and are removed
/// together.
///
/// Removing a run as a unit is what lets a `// TODO` followed by an
/// explanatory `// note` disappear in one edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommentRun {
    /// The attached comments, sorted by byte position.
    pub comments: Vec<Comment>,
}

impl CommentRun {
    /// The byte range covering every comment in the run.
    pub fn byte_range(&self) -> Range<usize> {
        let first = &self.comments[0];
        let last = &self.comments[self.comments.len() - 1];
        first.byte_range.start..last.byte_range.end
    }

    /// The 1-based line where the run starts.
    pub fn first_line(&self) -> usize {
        self.comments[0].line
    }
}

/// Which comments of a [`CommentRun`] the user chose to act on, as an
/// inclusive index range into `run.comments`.
///
/// The default is the full run; shrinking the selection is how the user
/// detaches a `TODO` from an attached comment they want to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Selection {
    /// First selected comment index (inclusive).
    pub start: usize,
    /// Last selected comment index (inclusive).
    pub end: usize,
}

impl Selection {
    /// The full selection of a run with `len` comments.
    ///
    /// An empty run yields an empty selection where `start > end`; the
    /// scanner never produces such runs.
    pub fn full(len: usize) -> Self {
        Self {
            start: 0,
            end: len.saturating_sub(1),
        }
    }

    /// Whether this selection covers every comment of a run with `len`
    /// comments.
    pub fn is_full(&self, len: usize) -> bool {
        len == 0 || (self.start == 0 && self.end + 1 == len)
    }

    /// The byte range of the selected comments within the file.
    ///
    /// # Panics
    ///
    /// Panics if the selection is out of bounds for `run` (the caller is
    /// responsible for clamping, see [`Self::clamp`]).
    pub fn byte_range(&self, run: &CommentRun) -> Range<usize> {
        let start = run.comments[self.start].byte_range.start;
        let end = run.comments[self.end].byte_range.end;
        start..end
    }

    /// Clamps the selection to a run with `len` comments, keeping at least
    /// the comment at index `anchor` selected.
    pub fn clamp(&mut self, len: usize, anchor: usize) {
        if len == 0 {
            return;
        }
        self.start = self.start.min(anchor).clamp(0, len - 1);
        self.end = self.end.max(anchor).clamp(0, len - 1);
    }
}

/// One actionable unit produced by the scanner: a comment run that contains a
/// category match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// The attached comment run the match belongs to.
    pub run: CommentRun,
    /// The category that matched, e.g. `TODO`.
    pub category: String,
    /// Index into `run.comments` of the first comment that matched.
    pub primary: usize,
    /// The comments the user chose to act on; defaults to the full run.
    pub selection: Selection,
}

impl Finding {
    /// The byte range of the currently selected comments.
    pub fn selected_range(&self) -> Range<usize> {
        self.selection.byte_range(&self.run)
    }

    /// The repository-relative path of the run.
    pub fn path(&self) -> &std::path::Path {
        &self.run.comments[0].path
    }

    /// The 1-based line of the primary (matched) comment.
    pub fn line(&self) -> usize {
        self.run.comments[self.primary].line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(start: usize, end: usize) -> Comment {
        Comment {
            path: PathBuf::from("src/lib.rs"),
            line: 1,
            end_line: 1,
            column: 0,
            byte_range: start..end,
            text: "// TODO: x".into(),
            language: "rust".into(),
        }
    }

    #[test]
    fn selection_full_and_clamp() {
        let mut sel = Selection { start: 0, end: 3 };
        assert!(sel.is_full(4));
        assert!(!sel.is_full(5));

        sel.start = 5;
        sel.clamp(4, 2);
        assert_eq!((sel.start, sel.end), (2, 3));

        sel = Selection { start: 0, end: 1 };
        sel.clamp(4, 3);
        assert_eq!((sel.start, sel.end), (0, 3));
    }

    #[test]
    fn selection_byte_range_covers_selected_comments() {
        let run = CommentRun {
            comments: vec![comment(0, 10), comment(11, 20), comment(21, 30)],
        };
        let sel = Selection { start: 1, end: 2 };
        assert_eq!(sel.byte_range(&run), 11..30);
    }

    #[test]
    fn run_byte_range_spans_first_to_last() {
        let run = CommentRun {
            comments: vec![comment(0, 10), comment(11, 20)],
        };
        assert_eq!(run.byte_range(), 0..20);
        assert_eq!(run.first_line(), 1);
    }

    #[test]
    fn finding_helpers() {
        let run = CommentRun {
            comments: vec![comment(0, 10)],
        };
        let finding = Finding {
            run,
            category: "TODO".into(),
            primary: 0,
            selection: Selection::full(1),
        };
        assert_eq!(finding.path(), PathBuf::from("src/lib.rs"));
        assert_eq!(finding.line(), 1);
        assert_eq!(finding.selected_range(), 0..10);
    }
}

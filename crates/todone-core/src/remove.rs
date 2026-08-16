//! Comment removal from source files.
//!
//! The scanner produces byte ranges; [`remove_selection`] turns a range into
//! edited source text, applying whitespace cleanup:
//!
//! - a comment that occupied a whole line removes that line (and one
//!   directly following blank line, so `// TODO` + blank does not leave a
//!   hole),
//! - inline comments are cut out and the surrounding whitespace is joined
//!   (`let x = 1; // TODO` becomes `let x = 1;`),
//! - line endings (`\n` vs `\r\n`) and the final newline are preserved.
//!
//! [`apply_removal`] additionally guards against clobbering edits made
//! since the scan: the file content must still match the snapshot recorded
//! when the finding was produced.

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// A snapshot of a file taken at scan time, used to detect concurrent edits
/// before a removal is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshot {
    /// Repository-relative path.
    pub path: PathBuf,
    /// SHA-256 hex digest of the file content at scan time.
    pub sha256: String,
}

impl FileSnapshot {
    /// Computes the snapshot of `path` by reading it from disk.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be read.
    pub fn capture(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            sha256: hex(&content),
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The outcome of a removal: the new content and the removed byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalOutcome {
    /// The edited source text.
    pub content: String,
    /// The byte range that was removed.
    pub removed_range: Range<usize>,
}

/// Errors produced while removing comments.
#[derive(Debug, Error)]
pub enum RemoveError {
    /// The selection is not within the file's bounds.
    #[error("selection {range:?} is out of bounds for file of {len} bytes")]
    OutOfBounds {
        /// The offending range.
        range: Range<usize>,
        /// The file size in bytes.
        len: usize,
    },
    /// The file changed since the scan snapshot was taken.
    #[error("file {path} changed since it was scanned; removal skipped")]
    Changed {
        /// The file that changed.
        path: PathBuf,
    },
    /// The file could not be read.
    #[error("failed to read {path}: {source}")]
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The edited file could not be written.
    #[error("failed to write {path}: {source}")]
    Write {
        /// The file that could not be written.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// Removes the selected byte ranges (comment nodes) from `source`, cleaning
/// up the affected lines.
///
/// Ranges may be disjoint (a run of comments separated by code); each is
/// clipped to its line, lines that become empty are removed, and inline
/// gaps are joined back together.
///
/// # Examples
///
/// ```
/// use todone_core::remove::remove_selection;
///
/// let src = "fn main() {\n    // TODO: fix this\n    let x = 1; // TODO: also this\n}\n";
/// let first_start = src.find("// TODO: fix this").unwrap();
/// let first = first_start..first_start + "// TODO: fix this".len();
/// let out = remove_selection(src, &[first]);
/// assert_eq!(out.content, "fn main() {\n    let x = 1; // TODO: also this\n}\n");
///
/// let second_start = out.content.find("// TODO: also this").unwrap();
/// let second = second_start..second_start + "// TODO: also this".len();
/// let out = remove_selection(&out.content, &[second]);
/// assert_eq!(out.content, "fn main() {\n    let x = 1;\n}\n");
/// ```
pub fn remove_selection(source: &str, ranges: &[Range<usize>]) -> RemovalOutcome {
    assert!(
        ranges
            .iter()
            .all(|r| r.start <= r.end && r.end <= source.len()),
        "selection ranges out of bounds for {} bytes",
        source.len()
    );

    let ranges = merge_ranges(ranges);
    if source.is_empty() || ranges.is_empty() {
        let range = ranges.first().cloned().unwrap_or(0..0);
        return RemovalOutcome {
            content: source.to_string(),
            removed_range: range,
        };
    }

    let mut lines = split_lines(source);
    let mut removed_lines: Vec<usize> = Vec::new();
    let mut removed_last_residue_line = false;

    // The index of the line where the last range ends; removing that line
    // triggers blank-line collapsing afterwards.
    let last_covered = last_line_index(&lines, ranges[ranges.len() - 1].end);

    for (index, line) in lines.iter_mut().enumerate() {
        let line_start = line.start;
        let line_end = line_start + line.text.len();
        // Ranges clipped to this line's text.
        let clipped: Vec<Range<usize>> = ranges
            .iter()
            .filter_map(|r| {
                let start = r.start.max(line_start).min(line_end);
                let end = r.end.max(line_start).min(line_end);
                (start < end).then_some(start..end)
            })
            .collect();
        if clipped.is_empty() {
            continue;
        }

        // Fragments of the line outside the covered ranges.
        let mut fragments: Vec<&str> = Vec::new();
        let mut cursor = line_start;
        for r in &clipped {
            if cursor < r.start {
                fragments.push(&source[cursor..r.start]);
            }
            cursor = r.end;
        }
        if cursor < line_end {
            fragments.push(&source[cursor..line_end]);
        }

        if fragments.iter().all(|f| f.trim().is_empty()) {
            removed_lines.push(index);
            removed_last_residue_line = index == last_covered;
        } else {
            let mut joined = String::new();
            for fragment in &fragments {
                joined = if joined.is_empty() {
                    (*fragment).to_string()
                } else {
                    join_fragments(&joined, fragment)
                };
            }
            line.text = joined.trim_end().to_string();
        }
    }

    if removed_last_residue_line {
        collapse_following_blank(&mut lines, &mut removed_lines);
    }

    for idx in removed_lines.iter().rev() {
        lines.remove(*idx);
    }

    let content = lines
        .iter()
        .map(|line| format!("{}{}", line.text, line.eol))
        .collect();

    RemovalOutcome {
        content,
        removed_range: ranges[0].start..ranges[ranges.len() - 1].end,
    }
}

/// The index of the line containing a byte offset (the last line as a
/// fallback).
fn last_line_index(lines: &[LineBuf], offset: usize) -> usize {
    lines
        .iter()
        .position(|line| offset <= line.start + line.len())
        .unwrap_or(lines.len().saturating_sub(1))
}

/// Sorts and merges overlapping or adjacent ranges.
fn merge_ranges(ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = ranges.to_vec();
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => {
                last.end = last.end.max(range.end);
            }
            _ => merged.push(range),
        }
    }
    merged
}

/// Joins two fragments of a line left by a comment removal.
///
/// - An empty first fragment keeps the second as-is (the comment started at
///   the line start, so indentation is preserved).
/// - An empty second fragment trims the first (trailing comment).
/// - Otherwise a space is inserted, except when the cut sits inside
///   brackets (`x(/* TODO */);` → `x();`).
fn join_fragments(before: &str, after: &str) -> String {
    if before.trim().is_empty() {
        return after.to_string();
    }
    if after.trim().is_empty() {
        return before.trim_end().to_string();
    }
    let before = before.trim_end();
    let after = after.trim_start();
    let inside_brackets = matches!(before.chars().last(), Some('(' | '[' | '{' | ','))
        && matches!(after.chars().next(), Some(')' | ']' | '}' | ',' | ';'));
    if inside_brackets {
        format!("{before}{after}")
    } else {
        format!("{before} {after}")
    }
}

/// Removes the blank line directly after a removed whole line, so
/// `// TODO\n\ncode` collapses to `code` instead of leaving a hole.
fn collapse_following_blank(lines: &mut [LineBuf], removed_lines: &mut Vec<usize>) {
    let Some(last) = removed_lines.last().copied() else {
        return;
    };
    let Some(next) = last.checked_add(1) else {
        return;
    };
    if next < lines.len() && lines[next].text.trim().is_empty() {
        removed_lines.push(next);
    }
}

/// One source line: text without the terminator, the terminator itself, and
/// the byte offset where the line starts in the original source.
struct LineBuf {
    text: String,
    eol: String,
    start: usize,
}

impl LineBuf {
    fn len(&self) -> usize {
        self.text.len() + self.eol.len()
    }
}

fn split_lines(source: &str) -> Vec<LineBuf> {
    let mut lines = Vec::new();
    let mut offset = 0;
    let mut rest = source;
    while !rest.is_empty() {
        match rest.find('\n') {
            Some(i) => {
                let (line_with_eol, tail) = rest.split_at(i + 1);
                let (text, eol) = match line_with_eol.strip_suffix("\r\n") {
                    Some(t) => (t, "\r\n"),
                    None => (line_with_eol.strip_suffix('\n').unwrap(), "\n"),
                };
                lines.push(LineBuf {
                    text: text.to_string(),
                    eol: eol.to_string(),
                    start: offset,
                });
                offset += line_with_eol.len();
                rest = tail;
            }
            None => {
                lines.push(LineBuf {
                    text: rest.to_string(),
                    eol: String::new(),
                    start: offset,
                });
                break;
            }
        }
    }
    lines
}

/// Removes the selection from the file on disk, verifying the file still
/// matches `expected` first.
///
/// The write is atomic (temp file + rename), so a crash never leaves a
/// partially written file.
///
/// # Errors
///
/// Returns [`RemoveError::Changed`] when the file no longer matches the
/// snapshot, [`RemoveError::OutOfBounds`] for invalid selections, and
/// read/write errors as [`RemoveError::Read`]/[`RemoveError::Write`].
pub fn apply_removal(
    path: &Path,
    ranges: &[Range<usize>],
    expected: &FileSnapshot,
) -> Result<RemovalOutcome, RemoveError> {
    let bytes = std::fs::read(path).map_err(|source| RemoveError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if hex(&bytes) != expected.sha256 {
        return Err(RemoveError::Changed {
            path: path.to_path_buf(),
        });
    }
    let source = std::str::from_utf8(&bytes).map_err(|_| RemoveError::Changed {
        path: path.to_path_buf(),
    })?;
    if let Some(range) = ranges
        .iter()
        .find(|r| r.start > r.end || r.end > source.len())
    {
        return Err(RemoveError::OutOfBounds {
            range: range.clone(),
            len: source.len(),
        });
    }
    let outcome = remove_selection(source, ranges);
    atomic_write(path, outcome.content.as_bytes()).map_err(|source| RemoveError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(outcome)
}

fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, content)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(src: &str, needle: &str) -> Range<usize> {
        let start = src
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in {src:?}"));
        start..start + needle.len()
    }

    fn remove(src: &str, needle: &str) -> String {
        let out = remove_selection(src, &[range(src, needle)]);
        // Removing again is always safe: the outcome must still be valid
        // input for a removal of the same range.
        assert!(out.removed_range == range(src, needle));
        out.content
    }

    #[test]
    fn standalone_line_comment_removes_the_line() {
        assert_eq!(remove("// TODO: x\ncode\n", "// TODO: x"), "code\n");
        assert_eq!(remove("    // TODO: x\ncode\n", "// TODO: x"), "code\n");
        assert_eq!(remove("\t// TODO: x\ncode\n", "// TODO: x"), "code\n");
        assert_eq!(remove("// TODO: x\n", "// TODO: x"), "");
        assert_eq!(remove("// TODO: x", "// TODO: x"), "");
    }

    #[test]
    fn comment_with_trailing_code_keeps_the_code() {
        assert_eq!(
            remove("let x = 1; // TODO: x\n", "// TODO: x"),
            "let x = 1;\n"
        );
        assert_eq!(remove("let x = 1; // TODO: x", "// TODO: x"), "let x = 1;");
    }

    #[test]
    fn inline_block_comment_is_joined() {
        assert_eq!(
            remove("x(); /* TODO */ y();\n", "/* TODO */"),
            "x(); y();\n"
        );
        assert_eq!(remove("x(/* TODO */);\n", "/* TODO */"), "x();\n");
        assert_eq!(
            remove("let a = /* TODO */ 1;\n", "/* TODO */"),
            "let a = 1;\n"
        );
        assert_eq!(remove("let a = /* TODO */ 1;", "/* TODO */"), "let a = 1;");
    }

    #[test]
    fn following_blank_line_is_collapsed() {
        assert_eq!(remove("// TODO: x\n\ncode\n", "// TODO: x"), "code\n");
        assert_eq!(remove("// TODO: x\n\n\ncode\n", "// TODO: x"), "\ncode\n");
        assert_eq!(
            remove("code\n\n// TODO: x\ncode2\n", "// TODO: x"),
            "code\n\ncode2\n"
        );
        assert_eq!(
            remove("// TODO: x\r\n\r\ncode\r\n", "// TODO: x"),
            "code\r\n"
        );
    }

    #[test]
    fn crlf_and_no_final_newline_are_preserved() {
        assert_eq!(remove("// TODO: x\r\ncode\r\n", "// TODO: x"), "code\r\n");
        assert_eq!(
            remove("let x = 1; // TODO: x\r\n", "// TODO: x"),
            "let x = 1;\r\n"
        );
        assert_eq!(remove("code\n// TODO: x", "// TODO: x"), "code\n");
    }

    #[test]
    fn multi_line_block_comment_is_removed_cleanly() {
        let src = "// start\n/* TODO: a\n * more\n */\ncode\n";
        assert_eq!(remove(src, "/* TODO: a\n * more\n */"), "// start\ncode\n");
        let src = "let x = /* TODO: a\n * more\n */ 5;\n";
        assert_eq!(remove(src, "/* TODO: a\n * more\n */"), "let x =\n 5;\n");
    }

    #[test]
    fn consecutive_line_comments_are_removed_as_a_run() {
        let src = "// TODO: a\n// note: attached\ncode\n";
        assert_eq!(remove(src, "// TODO: a\n// note: attached"), "code\n");
    }

    #[test]
    fn run_ending_in_a_trailing_comment_keeps_the_code() {
        let src = "// TODO: a\n// TODO: b\nlet x = 1; // TODO: c\n";
        // The run groups all three comment lines; removing the comment
        // nodes must keep the code that precedes the trailing comment.
        let a = range(src, "// TODO: a\n");
        let b = range(src, "// TODO: b\n");
        let c = range(src, "// TODO: c");
        let out = remove_selection(src, &[a, b, c]);
        assert_eq!(out.content, "let x = 1;\n");
    }

    #[test]
    fn run_starting_with_an_inline_comment_keeps_the_first_line_code() {
        let src = "let x = 1; // TODO: a\n// TODO: b\n}\n";
        let a = range(src, "// TODO: a");
        let b = range(src, "// TODO: b\n");
        let out = remove_selection(src, &[a, b]);
        assert_eq!(out.content, "let x = 1;\n}\n");
    }

    #[test]
    fn disjoint_ranges_on_one_line_are_joined() {
        let src = "let a = 1; // TODO: a\nlet b = 2; // TODO: b\n";
        let a = range(src, "// TODO: a");
        let b = range(src, "// TODO: b");
        let out = remove_selection(src, &[a, b]);
        assert_eq!(out.content, "let a = 1;\nlet b = 2;\n");
    }

    #[test]
    fn subset_selection_keeps_the_rest() {
        let src = "// TODO: a\n// keep me\ncode\n";
        // Select only the first comment.
        let range = range(src, "// TODO: a");
        let out = remove_selection(src, &[range]);
        assert_eq!(out.content, "// keep me\ncode\n");
    }

    #[test]
    fn empty_selection_is_a_no_op() {
        let src = "// TODO: x\ncode\n";
        let ranges = std::slice::from_ref(&(0..0));
        let out = remove_selection(src, ranges);
        assert_eq!(out.content, src);
    }

    #[test]
    fn apply_removal_writes_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "// TODO: x\ncode\n").unwrap();
        let snapshot = FileSnapshot::capture(&path).unwrap();

        let outcome = apply_removal(
            &path,
            &[range("// TODO: x\ncode\n", "// TODO: x")],
            &snapshot,
        )
        .unwrap();
        assert_eq!(outcome.content, "code\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "code\n");
    }

    #[test]
    fn apply_removal_rejects_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "// TODO: x\ncode\n").unwrap();
        let snapshot = FileSnapshot::capture(&path).unwrap();

        std::fs::write(&path, "// TODO: x\nchanged!\n").unwrap();
        let ranges = std::slice::from_ref(&(0..10));
        let err = apply_removal(&path, ranges, &snapshot).unwrap_err();
        assert!(matches!(err, RemoveError::Changed { .. }));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "// TODO: x\nchanged!\n"
        );
    }

    #[test]
    fn apply_removal_rejects_out_of_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "code\n").unwrap();
        let snapshot = FileSnapshot::capture(&path).unwrap();
        let ranges = std::slice::from_ref(&(5..9));
        let err = apply_removal(&path, ranges, &snapshot).unwrap_err();
        assert!(matches!(err, RemoveError::OutOfBounds { .. }));
    }

    #[test]
    fn snapshot_hex_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "one\n").unwrap();
        let a = FileSnapshot::capture(&path).unwrap();
        std::fs::write(&path, "two\n").unwrap();
        let b = FileSnapshot::capture(&path).unwrap();
        assert_ne!(a.sha256, b.sha256);
        assert!(a.sha256.len() == 64);
    }
}

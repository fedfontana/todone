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

/// Removes `selection` (a byte range) from `source`, cleaning up the
/// affected lines.
///
/// # Examples
///
/// ```
/// use todone_core::remove::remove_selection;
///
/// let src = "fn main() {\n    // TODO: fix this\n    let x = 1; // TODO: also this\n}\n";
/// let first_start = src.find("// TODO: fix this").unwrap();
/// let first = first_start..first_start + "// TODO: fix this".len();
/// let out = remove_selection(src, &first);
/// assert_eq!(out.content, "fn main() {\n    let x = 1; // TODO: also this\n}\n");
///
/// let second_start = out.content.find("// TODO: also this").unwrap();
/// let second = second_start..second_start + "// TODO: also this".len();
/// let out = remove_selection(&out.content, &second);
/// assert_eq!(out.content, "fn main() {\n    let x = 1;\n}\n");
/// ```
pub fn remove_selection(source: &str, selection: &Range<usize>) -> RemovalOutcome {
    assert!(
        selection.start <= selection.end && selection.end <= source.len(),
        "selection {:?} out of bounds for {} bytes",
        selection,
        source.len()
    );

    if source.is_empty() || selection.start == selection.end {
        return RemovalOutcome {
            content: source.to_string(),
            removed_range: selection.clone(),
        };
    }

    let mut lines = split_lines(source);
    let (first_idx, last_idx) = line_indices(&lines, selection);

    let first_start = lines[first_idx].start;
    let last_start = lines[last_idx].start;

    let before = &source[first_start..selection.start];
    let after = &source[selection.end..last_start + lines[last_idx].text.len()];

    let mut removed_lines: Vec<usize> = Vec::new();
    let mut removed_last_residue_line = false;

    if first_idx == last_idx {
        let combined = format!("{before}{after}");
        if combined.trim().is_empty() {
            removed_lines.push(first_idx);
            removed_last_residue_line = true;
        } else {
            lines[first_idx].text = inline_join(before, after);
            lines[first_idx].text = lines[first_idx].text.trim_end().to_string();
        }
    } else {
        if before.trim().is_empty() {
            removed_lines.push(first_idx);
        } else {
            lines[first_idx].text = before.trim_end().to_string();
        }
        for idx in first_idx + 1..last_idx {
            removed_lines.push(idx);
        }
        if after.trim().is_empty() {
            removed_lines.push(last_idx);
            removed_last_residue_line = true;
        } else {
            lines[last_idx].text = after.to_string();
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
        removed_range: selection.clone(),
    }
}

/// Joins the fragments left on a line by an inline comment removal.
///
/// A space is inserted only where both fragments look like they belong to
/// distinct tokens: `let x = 1; // TODO` → `let x = 1;`. When the cut sits
/// inside brackets (`x(/* TODO */);`) the fragments are joined directly, so
/// the result is `x();` rather than `x( );`.
fn inline_join(before: &str, after: &str) -> String {
    let before = before.trim_end();
    let after = after.trim_start();
    match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (true, false) => after.to_string(),
        (false, true) => before.to_string(),
        (false, false) => {
            let inside_brackets = matches!(before.chars().last(), Some('(' | '[' | '{' | ','))
                && matches!(after.chars().next(), Some(')' | ']' | '}' | ',' | ';'));
            if inside_brackets {
                format!("{before}{after}")
            } else {
                format!("{before} {after}")
            }
        }
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

/// Maps the selection's byte offsets to first and last line indices.
fn line_indices(lines: &[LineBuf], selection: &Range<usize>) -> (usize, usize) {
    let first = lines
        .iter()
        .position(|line| selection.start < line.start + line.len())
        .unwrap_or(lines.len() - 1);
    let last = lines
        .iter()
        .position(|line| selection.end <= line.start + line.len())
        .unwrap_or(lines.len() - 1);
    (first, last)
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
    selection: &Range<usize>,
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
    if selection.start > selection.end || selection.end > source.len() {
        return Err(RemoveError::OutOfBounds {
            range: selection.clone(),
            len: source.len(),
        });
    }
    let outcome = remove_selection(source, selection);
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
        let out = remove_selection(src, &range(src, needle));
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
    fn subset_selection_keeps_the_rest() {
        let src = "// TODO: a\n// keep me\ncode\n";
        // Select only the first comment.
        let range = range(src, "// TODO: a");
        let out = remove_selection(src, &range);
        assert_eq!(out.content, "// keep me\ncode\n");
    }

    #[test]
    fn empty_selection_is_a_no_op() {
        let src = "// TODO: x\ncode\n";
        let out = remove_selection(src, &(0..0));
        assert_eq!(out.content, src);
    }

    #[test]
    fn apply_removal_writes_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "// TODO: x\ncode\n").unwrap();
        let snapshot = FileSnapshot::capture(&path).unwrap();

        let outcome =
            apply_removal(&path, &range("// TODO: x\ncode\n", "// TODO: x"), &snapshot).unwrap();
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
        let err = apply_removal(&path, &(0..10), &snapshot).unwrap_err();
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
        let err = apply_removal(&path, &(5..9), &snapshot).unwrap_err();
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

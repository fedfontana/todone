//! Repository scanning: walking files, parsing them with tree-sitter, and
//! producing findings for marker comments.
//!
//! The scanner never writes anything; it is a pure read-only pass. All
//! findings carry the byte ranges needed to later remove comments.
//!
//! A comment node may span several lines. It is split into "visual" comments
//! at blank lines (blank-only markers like a bare `//` are a boundary and are
//! dropped), and every comment that matches a category becomes its own
//! finding — attached neighbours travel with the finding whose match they
//! surround, so a `// TODO` followed by an explanatory line is removed as a
//! pair while two adjacent `// TODO`s stay independent.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use thiserror::Error;
use tree_sitter::Parser;

use crate::language::{self, Language};
use crate::matcher::{MatchConfig, Matcher};
use crate::model::{Comment, CommentRun, Finding, Selection};

/// Options controlling a scan pass.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Paths relative to the repo root to scan; may name files or
    /// directories. Empty means the whole repo.
    pub paths: Vec<PathBuf>,
    /// Glob patterns (relative to the repo root) to exclude from scanning,
    /// e.g. `vendor/**`.
    pub exclude: Vec<String>,
    /// Which comments count as marker comments.
    pub match_config: MatchConfig,
    /// Files larger than this many bytes are skipped.
    pub max_file_bytes: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            exclude: Vec::new(),
            match_config: MatchConfig::default(),
            max_file_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Aggregate counters for a scan pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ScanStats {
    /// Files that were parsed.
    pub files: usize,
    /// Findings produced.
    pub findings: usize,
    /// Files skipped because they are not valid UTF-8.
    pub skipped_non_utf8: usize,
    /// Files skipped because they exceed `ScanOptions::max_file_bytes`.
    pub skipped_too_large: usize,
    /// Files skipped because no grammar is registered for them.
    pub skipped_unsupported: usize,
    /// Files skipped because their language's grammar could not be loaded.
    pub skipped_grammar: usize,
}

/// The outcome of a scan pass: findings plus statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    /// Every finding, sorted by path then line.
    pub findings: Vec<Finding>,
    /// Statistics about what was scanned and skipped.
    pub stats: ScanStats,
}

/// Errors produced while scanning.
#[derive(Debug, Error)]
pub enum ScanError {
    /// A file could not be read from disk.
    #[error("failed to read {path}: {source}")]
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// A file could not be parsed with its language's grammar.
    #[error("failed to parse {path} as {language}")]
    Parse {
        /// The file that could not be parsed.
        path: PathBuf,
        /// The language id used.
        language: &'static str,
    },
    /// A language's grammar could not be loaded (e.g. offline with an empty
    /// cache); the file is skipped rather than failing the scan.
    #[error("failed to load grammar for {language}: {detail}")]
    Grammar {
        /// The language id.
        language: String,
        /// The underlying error.
        detail: String,
    },
    /// The category configuration is invalid.
    #[error(transparent)]
    MatchConfig(#[from] crate::matcher::MatchConfigError),
}

/// Scans repositories for marker comments.
///
/// Constructing a scanner compiles the [`MatchConfig`] once; [`Scanner::scan`]
/// can then be called repeatedly (e.g. after the user edits files).
#[derive(Debug)]
pub struct Scanner {
    matcher: Matcher,
    options: ScanOptions,
    /// Resolved grammars, keyed by language id.
    grammars: std::cell::RefCell<HashMap<&'static str, tree_sitter::Language>>,
}

impl Scanner {
    /// Creates a scanner for the given options.
    ///
    /// # Errors
    ///
    /// Returns an error if the options' match configuration is invalid.
    pub fn new(options: ScanOptions) -> Result<Self, ScanError> {
        let matcher = options.match_config.compile()?;
        Ok(Self {
            matcher,
            options,
            grammars: std::cell::RefCell::new(HashMap::new()),
        })
    }

    /// Scans the repository rooted at `root`, returning findings sorted by
    /// path then line.
    ///
    /// # Errors
    ///
    /// Returns an error if a file cannot be read or parsed.
    pub fn scan(&self, root: &Path) -> Result<ScanResult, ScanError> {
        let mut files = collect_files(root, &self.options);
        files.sort();
        files.dedup();

        let mut stats = ScanStats::default();
        let mut findings = Vec::new();

        for path in files {
            let Some(lang) = language::by_extension(&path) else {
                stats.skipped_unsupported += 1;
                continue;
            };
            let bytes = std::fs::read(&path).map_err(|source| ScanError::Read {
                path: path.clone(),
                source,
            })?;
            if bytes.len() > self.options.max_file_bytes {
                stats.skipped_too_large += 1;
                continue;
            }
            let Some(source) = std::str::from_utf8(&bytes).ok() else {
                stats.skipped_non_utf8 += 1;
                continue;
            };
            stats.files += 1;

            let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            let comments = match self.parse_comments(relative, lang, source) {
                Ok(comments) => comments,
                // Unavailable grammars skip the file; the rest of the scan
                // proceeds (e.g. offline with a cold cache).
                Err(ScanError::Grammar { .. }) => {
                    stats.skipped_grammar += 1;
                    continue;
                }
                Err(err) => return Err(err),
            };
            for run in group_runs(&comments) {
                for (category, primary, comments) in matched_segments(&run, &self.matcher) {
                    let len = comments.len();
                    let run = CommentRun { comments };
                    findings.push(Finding {
                        category,
                        run,
                        primary,
                        selection: Selection::full(len),
                    });
                }
            }
        }

        findings.sort_by(|a, b| a.path().cmp(b.path()).then_with(|| a.line().cmp(&b.line())));
        stats.findings = findings.len();

        Ok(ScanResult { findings, stats })
    }

    fn parse_comments(
        &self,
        path: PathBuf,
        lang: &Language,
        source: &str,
    ) -> Result<Vec<Comment>, ScanError> {
        let ts = self.grammar(lang.id)?;
        comments_from_tree(path, lang, source, &ts)
    }

    /// Resolves the grammar for a language id, caching it for the scanner's
    /// lifetime.
    fn grammar(&self, id: &'static str) -> Result<tree_sitter::Language, ScanError> {
        if let Some(ts) = self.grammars.borrow().get(id) {
            return Ok(ts.clone());
        }
        let ts = crate::language::grammar(id).map_err(|e| ScanError::Grammar {
            language: id.to_string(),
            detail: e.to_string(),
        })?;
        self.grammars.borrow_mut().insert(id, ts.clone());
        Ok(ts)
    }
}

/// Parses `source` with `lang`'s grammar and returns its comment entries,
/// already split at blank lines (see [`split_comment_node`]). Blank-only
/// markers are dropped.
///
/// This is the standalone entry point the CLI uses to re-derive comment
/// ranges (e.g. to scrub comments from draft snippets); the [`Scanner`]
/// keeps its own grammar cache and delegates to the same core.
///
/// # Errors
///
/// Returns a [`ScanError::Grammar`] when the grammar cannot be loaded and a
/// [`ScanError::Parse`] when `source` cannot be parsed with it.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use todone_core::language;
/// use todone_core::scan::parse_comment_nodes;
///
/// let lang = language::by_extension(Path::new("a.rs")).unwrap();
/// let comments = parse_comment_nodes(
///     Path::new("a.rs").into(),
///     lang,
///     "// TODO: fix\n",
/// ).unwrap();
/// assert_eq!(comments.len(), 1);
/// assert_eq!(comments[0].line, 1);
/// ```
pub fn parse_comment_nodes(
    path: PathBuf,
    lang: &Language,
    source: &str,
) -> Result<Vec<Comment>, ScanError> {
    let ts = match crate::language::grammar(lang.id) {
        Ok(ts) => ts,
        Err(crate::language::GrammarError::Load { detail, .. }) => {
            return Err(ScanError::Grammar {
                language: lang.id.to_string(),
                detail,
            });
        }
    };
    comments_from_tree(path, lang, source, &ts)
}

/// Parses `source` with a pre-resolved grammar, collecting every comment
/// node and splitting it at blank lines.
fn comments_from_tree(
    path: PathBuf,
    lang: &Language,
    source: &str,
    ts: &tree_sitter::Language,
) -> Result<Vec<Comment>, ScanError> {
    let mut parser = Parser::new();
    parser.set_language(ts).map_err(|_| ScanError::Parse {
        path: path.clone(),
        language: lang.id,
    })?;
    let tree = parser.parse(source, None).ok_or_else(|| ScanError::Parse {
        path: path.clone(),
        language: lang.id,
    })?;

    let mut comments = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if lang.comment_kinds.contains(&node.kind()) {
            comments.extend(split_comment_node(&path, lang, source, &node));
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    comments.sort_by_key(|c| c.byte_range.start);
    Ok(comments)
}

/// Whether a comment line carries no visible content: blank, or only the
/// marker decoration (`//`, `/*`, `*`, `#`, ...). Such lines separate
/// visual comments and are never part of a run.
fn is_visually_empty(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || trimmed
            .trim_start_matches(['/', '*', '#'])
            .trim()
            .is_empty()
}

/// The 0-based column of a file byte offset, counting the characters since
/// the start of its line.
fn source_column(source: &str, offset: usize) -> usize {
    let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    source[line_start..offset].chars().count()
}

/// One line of a comment node, with its byte offset within the node.
struct VisualLine<'a> {
    text: &'a str,
    start: usize,
    empty: bool,
}

/// Splits one comment node into "visual" [`Comment`] entries.
///
/// Blank lines within the node are boundaries: the entries before and after
/// them are reported separately (and, being separated by a line that no
/// entry owns, they never group into a single run). A node whose *entire*
/// content is blank (e.g. a bare `//`) produces no entry at all.
fn split_comment_node(
    path: &Path,
    lang: &Language,
    source: &str,
    node: &tree_sitter::Node,
) -> Vec<Comment> {
    let range = node.byte_range();
    let raw = &source[range.clone()];
    let raw = raw.strip_suffix('\n').unwrap_or(raw);

    let mut lines = Vec::new();
    let mut offset = 0usize;
    for text in raw.split('\n') {
        lines.push(VisualLine {
            text,
            start: offset,
            empty: is_visually_empty(text),
        });
        offset += text.len() + 1;
    }

    let start_row = node.start_position().row;
    let mut entries = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        if lines[start].empty {
            start += 1;
            continue;
        }
        let mut end = start;
        while end + 1 < lines.len() && !lines[end + 1].empty {
            end += 1;
        }

        let first = &lines[start];
        let last = &lines[end];
        let byte_range = range.start + first.start..range.start + last.start + last.text.len();
        entries.push(Comment {
            path: path.to_path_buf(),
            line: start_row + start + 1,
            end_line: start_row + end + 1,
            column: source_column(source, byte_range.start),
            byte_range,
            text: raw[first.start..last.start + last.text.len()].to_string(),
            language: lang.id.to_string(),
        });
        start = end + 1;
    }
    entries
}

/// Groups comments into maximal runs of adjacent nodes: a comment joins the
/// current run when it starts on the same or the next line of the run's end.
///
/// Blank lines never produce comments (see [`split_comment_node`]), so a run
/// cannot bridge across one.
fn group_runs(comments: &[Comment]) -> Vec<CommentRun> {
    let mut runs: Vec<CommentRun> = Vec::new();
    for comment in comments {
        let attached = runs.last().is_some_and(|run| {
            let last = run.comments.last().unwrap();
            comment.line <= last.end_line + 1
        });
        if attached {
            runs.last_mut().unwrap().comments.push(comment.clone());
        } else {
            runs.push(CommentRun {
                comments: vec![comment.clone()],
            });
        }
    }
    runs
}

/// Partitions an adjacent comment run into findings: every comment that
/// matches a category starts a new segment, so each segment holds exactly
/// one matched comment plus any attached neighbours. Non-matching comments
/// between two matches travel with the segment of the *preceding* match;
/// leading and trailing neighbours attach to the first and last segment.
///
/// The returned tuples are `(category, index of the matched comment within
/// the segment, the segment's comments)`.
fn matched_segments(run: &CommentRun, matcher: &Matcher) -> Vec<(String, usize, Vec<Comment>)> {
    /// One candidate segment: its comments plus the match it carries.
    struct Segment {
        comments: Vec<Comment>,
        category: Option<(String, usize)>,
    }

    let mut segments: Vec<Segment> = Vec::new();
    for comment in &run.comments {
        match matcher.match_category(&comment.text) {
            Some(category) => {
                if segments.is_empty() || segments.last().unwrap().category.is_some() {
                    segments.push(Segment {
                        comments: Vec::new(),
                        category: None,
                    });
                }
                let segment = segments.last_mut().unwrap();
                segment.comments.push(comment.clone());
                // The first match of a segment is its primary; later matches
                // already created their own segment.
                if segment.category.is_none() {
                    segment.category = Some((category.to_string(), segment.comments.len() - 1));
                }
            }
            None => {
                if segments.is_empty() {
                    segments.push(Segment {
                        comments: Vec::new(),
                        category: None,
                    });
                }
                segments.last_mut().unwrap().comments.push(comment.clone());
            }
        }
    }

    segments
        .into_iter()
        .filter_map(|segment| {
            segment
                .category
                .map(|(category, primary)| (category, primary, segment.comments))
        })
        .collect()
}

/// Walks the files covered by the scope paths.
///
/// - With an empty scope the whole tree under `root` is walked, honoring
///   `.gitignore` and skipping hidden entries (ripgrep defaults).
/// - With a scope, each path is walked individually; explicitly named files
///   are always included, even when hidden or ignored.
/// - Exclude globs (relative to `root`) prune matching subtrees.
fn collect_files(root: &Path, options: &ScanOptions) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if options.paths.is_empty() {
        walk_into(root, options, &mut files);
    } else {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        for rel in &options.paths {
            let target = root.join(rel);
            if target.is_file() {
                if seen.insert(target.clone()) {
                    files.push(target);
                }
                continue;
            }
            walk_into(&target, options, &mut files);
        }
    }
    files
}

fn walk_into(target: &Path, options: &ScanOptions, files: &mut Vec<PathBuf>) {
    let mut builder = WalkBuilder::new(target);
    builder
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .hidden(true)
        .parents(true)
        .follow_links(false);

    if !options.exclude.is_empty() {
        let mut overrides = ignore::overrides::OverrideBuilder::new(target);
        for pattern in &options.exclude {
            let _ = overrides.add(&format!("!{pattern}"));
        }
        if let Ok(overrides) = overrides.build() {
            builder.overrides(overrides);
        }
    }

    for entry in builder.build() {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            files.push(entry.into_path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    fn scan_dir(root: &Path, paths: Vec<PathBuf>) -> ScanResult {
        let options = ScanOptions {
            paths,
            ..Default::default()
        };
        Scanner::new(options).unwrap().scan(root).unwrap()
    }

    #[test]
    fn finds_todos_across_languages() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/main.rs",
            "fn main() {\n    // TODO: fix this\n    // FIXME: and this\n}\n",
        );
        write(
            root,
            "lib.py",
            "def f():\n    # TODO: python todo\n    pass\n",
        );
        write(
            root,
            "script.sh",
            "#!/usr/bin/env bash\n# TODO: shell todo\necho hi\n",
        );
        write(
            root,
            "main.c",
            "int main(void) {\n    // TODO: c todo\n    return 0;\n}\n",
        );
        write(
            root,
            "main.go",
            "package main\n// TODO: go todo\nfunc main() {}\n",
        );
        write(root, "app.ts", "// TODO: ts todo\nexport const x = 1;\n");
        write(root, "config.json", "{\"a\": 1}\n");

        let result = scan_dir(root, vec![]);
        let by_path: Vec<_> = result
            .findings
            .iter()
            .map(|f| {
                (
                    f.path().to_string_lossy().into_owned(),
                    f.category.clone(),
                    f.line(),
                )
            })
            .collect();

        assert_eq!(result.stats.skipped_unsupported, 0);
        assert!(by_path.contains(&("src/main.rs".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("src/main.rs".into(), "FIXME".into(), 3)));
        assert!(by_path.contains(&("lib.py".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("script.sh".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("main.c".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("main.go".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("app.ts".into(), "TODO".into(), 1)));
        // The adjacent TODO and FIXME now report as two findings.
        assert_eq!(result.findings.len(), 7);
    }

    #[test]
    fn each_matching_comment_is_its_own_finding() {
        // Adjacent marker comments never merge: every match is reported.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/main.rs", "// TODO: a\n// FIXME: b\n// TODO: c\n");
        let result = scan_dir(root, vec![]);
        let findings: Vec<_> = result
            .findings
            .iter()
            .map(|f| (f.category.clone(), f.line(), f.run.comments.len()))
            .collect();
        assert_eq!(
            findings,
            vec![
                ("TODO".to_string(), 1, 1),
                ("FIXME".to_string(), 2, 1),
                ("TODO".to_string(), 3, 1),
            ]
        );
        assert!(result.findings.iter().all(|f| f.selection.is_full(1)));
    }

    #[test]
    fn non_matching_neighbours_travel_with_their_match() {
        // A note between two markers rides with the preceding match, and no
        // segment ever carries two matches.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.rs", "// TODO: a\n// note\n// TODO: b\n");
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 2);
        let first = &result.findings[0];
        assert_eq!(first.category, "TODO");
        assert_eq!(first.run.comments.len(), 2);
        assert_eq!(first.run.comments[0].text, "// TODO: a");
        assert_eq!(first.run.comments[1].text, "// note");
        let second = &result.findings[1];
        assert_eq!(second.category, "TODO");
        assert_eq!(second.line(), 3);
        assert_eq!(second.run.comments.len(), 1);
    }

    #[test]
    fn blank_comment_line_splits_the_run() {
        // A blank `//` separates two comments: the continuation keeps the
        // TODO, the FIXME becomes its own finding.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "a.rs",
            "// TODO: something\n// and some continuation\n//\n// FIXME: some other\n",
        );
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 2);
        let first = &result.findings[0];
        assert_eq!(first.category, "TODO");
        assert_eq!(first.line(), 1);
        assert_eq!(first.run.comments.len(), 2);
        assert_eq!(first.run.comments[1].text, "// and some continuation");
        assert!(first.selection.is_full(2));
        let second = &result.findings[1];
        assert_eq!(second.category, "FIXME");
        assert_eq!(second.line(), 4);
        assert_eq!(second.run.comments.len(), 1);
    }

    #[test]
    fn blank_only_markers_are_ignored() {
        // A bare `//` is a boundary, never a finding of its own.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.rs", "//\n// TODO: real\n");
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].line(), 2);
    }

    #[test]
    fn block_comment_splits_at_blank_lines() {
        // A blank ` * ` line inside one block comment separates its markers.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.rs", "/* TODO: header\n *\n * FIXME: footer\n */\n");
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 2);
        assert_eq!(result.findings[0].category, "TODO");
        assert_eq!(result.findings[0].line(), 1);
        assert_eq!(result.findings[0].run.comments.len(), 1);
        assert_eq!(result.findings[1].category, "FIXME");
        assert_eq!(result.findings[1].line(), 3);
        assert_eq!(result.findings[1].run.comments.len(), 1);
    }

    #[test]
    fn block_comment_without_blank_lines_stays_one_finding() {
        // No blank line inside the block: it is one visual comment, reported
        // once even though two lines mention markers.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.rs", "/* TODO: a\n * FIXME: b\n */\n");
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].category, "TODO");
        assert_eq!(result.findings[0].line(), 1);
        assert_eq!(result.findings[0].run.comments.len(), 1);
    }

    #[test]
    fn trailing_comment_range_excludes_code_before_it() {
        // The selection covers only the comment: nothing before `//`.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.rs", "let x = 1; // TODO: fix\n");
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 1);
        let comment = &result.findings[0].run.comments[0];
        assert_eq!(comment.text, "// TODO: fix");
        assert_eq!(comment.column, 11);
        assert_eq!(comment.byte_range.start, 11);
        assert_eq!(result.findings[0].selected_range(), comment.byte_range);
    }

    #[test]
    fn strings_and_urls_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "lib.py",
            "url = \"http://TODO.example\"\n\ndef f():\n    \"\"\"TODO inside docstring is not a comment.\"\"\"\n    pass\n",
        );
        let result = scan_dir(root, vec![]);
        assert!(result.findings.is_empty());
        assert_eq!(result.stats.files, 1);
    }

    #[test]
    fn adjacent_comments_form_a_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "a.rs",
            "// TODO: first\n// attached note\nfn main() {\n    let x = 1; // TODO: trailing\n}\n",
        );
        let result = scan_dir(root, vec![]);
        let first = &result.findings[0];
        assert_eq!(first.run.comments.len(), 2);
        assert_eq!(first.category, "TODO");
        assert!(first.selection.is_full(2));

        let second = &result.findings[1];
        assert_eq!(second.run.comments.len(), 1);
        assert_eq!(second.line(), 4);
    }

    #[test]
    fn grouped_block_comment_with_inline_attachments() {
        // A block comment with no blank lines is one visual comment; the
        // next-line TODO matches too, so the run splits into two findings.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "a.rs",
            "/* TODO: header\n * more text\n */\n// TODO: next line\nfn main() {}\n",
        );
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 2);
        assert_eq!(result.findings[0].run.comments.len(), 1);
        assert_eq!(result.findings[0].line(), 1);
        assert_eq!(result.findings[1].run.comments.len(), 1);
        assert_eq!(result.findings[1].line(), 4);
    }

    #[test]
    fn scope_limits_scanning() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/a.rs", "// TODO: in src\n");
        write(root, "other/b.rs", "// TODO: in other\n");

        let result = scan_dir(root, vec![PathBuf::from("src")]);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].path(), Path::new("src/a.rs"));

        let result = scan_dir(root, vec![PathBuf::from("other/b.rs")]);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].path(), Path::new("other/b.rs"));
    }

    #[test]
    fn gitignore_and_hidden_are_respected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "keep.rs", "// TODO: kept\n");
        write(root, "vendor/ignored.rs", "// TODO: ignored by gitignore\n");
        write(root, ".gitignore", "vendor/\n");
        write(root, ".hidden/secret.rs", "// TODO: hidden\n");

        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].path(), Path::new("keep.rs"));
    }

    #[test]
    fn explicitly_named_hidden_file_is_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".hidden/secret.rs", "// TODO: hidden\n");

        let result = scan_dir(root, vec![PathBuf::from(".hidden/secret.rs")]);
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn exclude_patterns_prune_subtrees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/a.rs", "// TODO: keep\n");
        write(root, "vendor/ignored.rs", "// TODO: dropped\n");

        let options = ScanOptions {
            exclude: vec!["vendor/**".into()],
            ..Default::default()
        };
        let result = Scanner::new(options).unwrap().scan(root).unwrap();
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].path(), Path::new("src/a.rs"));
    }

    #[test]
    fn non_utf8_and_oversized_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("bin.rs"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
        let huge = "// TODO: big\n".repeat(3_000_000);
        fs::write(root.join("huge.rs"), huge).unwrap();

        let options = ScanOptions {
            max_file_bytes: 1024 * 1024,
            ..Default::default()
        };
        let result = Scanner::new(options).unwrap().scan(root).unwrap();
        assert_eq!(result.stats.skipped_non_utf8, 1);
        assert_eq!(result.stats.skipped_too_large, 1);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn unsupported_extensions_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "notes.txt", "TODO: not a code comment\n");
        let result = scan_dir(root, vec![]);
        assert_eq!(result.stats.skipped_unsupported, 1);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn findings_are_sorted_by_path_then_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "b.rs", "// TODO: b1\n\n// TODO: b2\n");
        write(root, "a.rs", "// TODO: a\n");
        let result = scan_dir(root, vec![]);
        let paths: Vec<_> = result
            .findings
            .iter()
            .map(|f| (f.path().to_string_lossy().into_owned(), f.line()))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("a.rs".to_string(), 1),
                ("b.rs".to_string(), 1),
                ("b.rs".to_string(), 3),
            ]
        );
    }

    #[test]
    fn invalid_match_config_is_rejected() {
        let options = ScanOptions {
            match_config: MatchConfig {
                categories: vec![],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(Scanner::new(options).is_err());
    }
}

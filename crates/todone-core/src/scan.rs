//! Repository scanning: walking files, parsing them with tree-sitter, and
//! producing findings for marker comments.
//!
//! The scanner never writes anything; it is a pure read-only pass. All
//! findings carry the byte ranges needed to later remove comments.

use std::collections::HashSet;
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
}

impl Scanner {
    /// Creates a scanner for the given options.
    ///
    /// # Errors
    ///
    /// Returns an error if the options' match configuration is invalid.
    pub fn new(options: ScanOptions) -> Result<Self, ScanError> {
        let matcher = options.match_config.compile()?;
        Ok(Self { matcher, options })
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
            let comments = self
                .parse_comments(relative, lang, source)
                .map_err(|language| ScanError::Parse {
                    path: path.clone(),
                    language,
                })?;
            for run in group_runs(&comments) {
                if let Some((primary, category)) = first_match(&run, &self.matcher) {
                    let selection = Selection::full(run.comments.len());
                    findings.push(Finding {
                        category: category.to_string(),
                        run,
                        primary,
                        selection,
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
    ) -> Result<Vec<Comment>, &'static str> {
        let mut parser = Parser::new();
        parser.set_language(&lang.ts()).map_err(|_| lang.id)?;
        let tree = parser.parse(source, None).ok_or(lang.id)?;

        let mut comments = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if lang.comment_kinds.contains(&node.kind()) {
                let range = node.byte_range();
                // Line-comment tokens include the trailing newline; the text
                // field stops before it and end_line names the last line
                // that actually carries comment text.
                let raw = &source[range.clone()];
                let (text, end_line) = match raw.strip_suffix('\n') {
                    Some(body) => (body, node.end_position().row),
                    None => (raw, node.end_position().row + 1),
                };
                comments.push(Comment {
                    path: path.clone(),
                    line: node.start_position().row + 1,
                    end_line,
                    column: node.start_position().column,
                    byte_range: range.clone(),
                    text: text.to_string(),
                    language: lang.id.to_string(),
                });
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        comments.sort_by_key(|c| c.byte_range.start);
        Ok(comments)
    }
}

/// Groups comments into maximal runs of adjacent nodes: a comment joins the
/// current run when it starts on the same or the next line of the run's end.
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

/// Returns the index and category of the first comment in the run that
/// matches the configured categories.
fn first_match<'a>(run: &CommentRun, matcher: &'a Matcher) -> Option<(usize, &'a str)> {
    run.comments.iter().enumerate().find_map(|(i, comment)| {
        matcher
            .match_category(&comment.text)
            .map(|category| (i, category))
    })
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
        assert!(by_path.contains(&("lib.py".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("script.sh".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("main.c".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("main.go".into(), "TODO".into(), 2)));
        assert!(by_path.contains(&("app.ts".into(), "TODO".into(), 1)));
        assert_eq!(result.findings.len(), 6);
    }

    #[test]
    fn adjacent_run_matches_only_the_first_category() {
        // The FIXME on the next line is attached to the TODO run; the run is
        // reported once, under the first matching category.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/main.rs",
            "// TODO: fix this\n// FIXME: and this\n",
        );
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 1);
        let finding = &result.findings[0];
        assert_eq!(finding.category, "TODO");
        assert_eq!(finding.run.comments.len(), 2);
        assert_eq!(finding.run.comments[1].text, "// FIXME: and this");
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
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "a.rs",
            "/* TODO: header\n * more text\n */\n// TODO: next line\nfn main() {}\n",
        );
        let result = scan_dir(root, vec![]);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].run.comments.len(), 2);
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

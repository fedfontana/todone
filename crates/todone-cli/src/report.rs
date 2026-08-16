//! JSON report emitted by `todone scan --json`.

use std::ops::Range;
use std::path::PathBuf;

use serde::Serialize;
use todone_core::scan::ScanStats;

use crate::context::LineKind;

/// The machine-readable scan report.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    /// The repository the scan ran in.
    pub repo: RepoReport,
    /// Every finding, in scan order.
    pub findings: Vec<FindingReport>,
    /// Aggregate counters.
    pub stats: ScanStats,
}

/// Repository metadata for reports.
#[derive(Debug, Clone, Serialize)]
pub struct RepoReport {
    /// Absolute path of the repository root.
    pub root: PathBuf,
    /// `HEAD` commit hash, when the repo has commits.
    pub commit: Option<String>,
    /// Whether the root is an actual git repository.
    pub is_repo: bool,
    /// The `origin` remote URL, when the repo has one.
    pub remote: Option<String>,
}

/// One finding, with its context window.
#[derive(Debug, Clone, Serialize)]
pub struct FindingReport {
    /// Repository-relative path.
    pub path: PathBuf,
    /// Language id.
    pub language: String,
    /// The matched category.
    pub category: String,
    /// 1-based line of the primary (matched) comment.
    pub line: usize,
    /// Byte range of the selected comments.
    pub selection: Range<usize>,
    /// All comments of the run, in order.
    pub comments: Vec<CommentReport>,
    /// Index into `comments` of the primary comment.
    pub primary: usize,
    /// The context window.
    pub context: Vec<ContextReport>,
}

/// One comment of the run.
#[derive(Debug, Clone, Serialize)]
pub struct CommentReport {
    /// 1-based start line.
    pub line: usize,
    /// 1-based end line.
    pub end_line: usize,
    /// Byte range within the file.
    pub range: Range<usize>,
    /// The comment text, including its marker.
    pub text: String,
}

/// One context line.
#[derive(Debug, Clone, Serialize)]
pub struct ContextReport {
    /// 1-based line number.
    pub line: usize,
    /// The line text.
    pub text: String,
    /// Role of the line.
    pub kind: LineKind,
}

impl FindingReport {
    /// Converts a core finding plus its context into the report shape.
    pub fn new(finding: &todone_core::model::Finding, context: &crate::context::Context) -> Self {
        let primary_comment = &finding.run.comments[finding.primary];
        Self {
            path: finding.path().to_path_buf(),
            language: primary_comment.language.clone(),
            category: finding.category.clone(),
            line: primary_comment.line,
            selection: finding.selected_range(),
            comments: finding
                .run
                .comments
                .iter()
                .map(|c| CommentReport {
                    line: c.line,
                    end_line: c.end_line,
                    range: c.byte_range.clone(),
                    text: c.text.clone(),
                })
                .collect(),
            primary: finding.primary,
            context: context
                .lines
                .iter()
                .map(|l| ContextReport {
                    line: l.line,
                    text: l.text.clone(),
                    kind: l.kind,
                })
                .collect(),
        }
    }
}

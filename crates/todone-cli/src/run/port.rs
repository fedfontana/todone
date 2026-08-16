//! Execution of the `port` subcommand: interactive review, draft editing,
//! confirmation, issue creation, and comment removal.
//!
//! Write discipline: no forge call and no file edit happens before the user
//! confirms, and a comment is only removed after its issue was created on
//! the forge. [`execute`] encodes that invariant and is fully testable with
//! a fake forge and no TTY.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context as _;
use serde::Serialize;
use todone_core::draft::IssueDraft;
use todone_core::model::Finding;
use todone_core::remove::{FileSnapshot, apply_removal};
use todone_core::scan::Scanner;
use todone_core::session::{Decision, Session};
use todone_forge::forge::Forge;

use crate::cli::{AutoDecision, PortArgs};
use crate::editor::Editor;
use crate::run::scan::ScanContext;
use crate::tui::{AppAction, PortApp};

/// Runs the port flow: scan, review (interactive or `--auto`), confirm
/// (unless `--yes`), then execute or print the plan (`--dry-run`).
///
/// # Errors
///
/// Returns an error when scanning, editing, or execution fails.
pub fn run_port(
    out: &mut dyn Write,
    context: &ScanContext,
    args: &PortArgs,
    json: bool,
    _color: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        context.repo.is_repo,
        "not in a git repository; nothing to port (scan and config still work)"
    );
    let scanner = Scanner::new(context.config.scan_options())?;
    let result = scanner.scan(&context.repo.root)?;

    if result.findings.is_empty() {
        writeln!(out, "no marker comments found")?;
        return Ok(());
    }

    let commit = context.repo.commit.clone().unwrap_or_else(|| "HEAD".into());
    let mut session = Session::new(result.findings, commit);
    let snapshots = capture_snapshots(&session, &context.repo.root)?;

    if let Some(auto) = args.auto {
        auto_decide(&mut session, auto);
    }

    let mut app = PortApp::new(session, context);
    app.dry_run = args.dry_run;

    // Phase 1: decisions.
    if args.auto.is_none() {
        app.auto_confirm = args.yes;
        let editor = Editor::resolve(&context.config.editor);
        let action = crate::tui::run_interactive(&mut app, &editor)?;
        if action == AppAction::Quit {
            writeln!(out, "aborted; nothing was created or removed")?;
            return Ok(());
        }
    }

    // Phase 2: execute or print the plan.
    if args.dry_run {
        print_plan(out, &app.session)?;
        return Ok(());
    }

    let forge = todone_forge::forge::from_config(
        &context.config.forge,
        Box::new(todone_forge::process::SystemProcessRunner),
    )?;
    let results = execute(&app.session, forge.as_ref(), &snapshots, &context.repo.root);
    print_execution(out, &app.session, &results, json)
}

/// Captures a snapshot per file referenced by the session, at scan time.
fn capture_snapshots(
    session: &Session,
    root: &std::path::Path,
) -> anyhow::Result<HashMap<PathBuf, FileSnapshot>> {
    let mut snapshots = HashMap::new();
    for finding in &session.findings {
        let path = finding.path();
        if !snapshots.contains_key(path) {
            let snapshot = FileSnapshot::capture(&root.join(path))
                .with_context(|| format!("failed to snapshot {}", path.display()))?;
            snapshots.insert(path.to_path_buf(), snapshot);
        }
    }
    Ok(snapshots)
}

/// Fills decisions for `--auto` mode.
fn auto_decide(session: &mut Session, auto: AutoDecision) {
    let commit = session.commit.clone();
    let drafts: Vec<IssueDraft> = session
        .findings
        .iter()
        .map(|finding| auto_draft(finding, &commit))
        .collect();
    for (index, draft) in drafts.into_iter().enumerate() {
        let decision = match auto {
            AutoDecision::Skip => Decision::Skip,
            AutoDecision::Delete => Decision::Delete,
            AutoDecision::Port => Decision::Port(draft),
        };
        session.set_decision(index, decision);
    }
}

/// Builds a draft from the comment text, for `--auto port`.
fn auto_draft(finding: &Finding, commit: &str) -> IssueDraft {
    let primary = &finding.run.comments[finding.primary];
    let mut title: String = primary.text.lines().next().unwrap_or("").trim().to_string();
    if title.len() > 72 {
        title.truncate(72);
        title.push_str("...");
    }
    IssueDraft {
        category: finding.category.clone(),
        path: finding.path().to_path_buf(),
        commit: commit.to_string(),
        title,
        description: format!(
            "Ported from `{}:{}`\n\n```\n{}\n```\n",
            finding.path().display(),
            finding.line(),
            primary.text.trim()
        ),
    }
}

/// What happened to one finding during execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionResult {
    /// Finding index in the session.
    pub index: usize,
    /// The action taken.
    pub action: &'static str,
    /// The created issue, when the action was a port and it succeeded.
    pub issue: Option<IssueJson>,
    /// Whether the comment was removed from the file.
    pub removed: bool,
    /// A human-readable error, when the step failed.
    pub error: Option<String>,
}

/// The machine-readable shape of a created issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssueJson {
    /// Issue number.
    pub number: u64,
    /// Issue URL.
    pub url: String,
}

/// Executes the session against `forge`, applying removals through the
/// repository at `root`. Ports create the issue first; the comment is only
/// removed after the issue exists. Deletes remove the comment directly.
pub fn execute(
    session: &Session,
    forge: &dyn Forge,
    snapshots: &HashMap<PathBuf, FileSnapshot>,
    root: &std::path::Path,
) -> Vec<ExecutionResult> {
    let mut results = Vec::new();
    for (index, finding) in session.findings.iter().enumerate() {
        let mut result = ExecutionResult {
            index,
            action: "skip",
            issue: None,
            removed: false,
            error: None,
        };
        match session.decision(index) {
            Some(Decision::Skip) | None => {}
            Some(Decision::Delete) => {
                result.action = "delete";
                match remove(finding, root, snapshots) {
                    Ok(()) => result.removed = true,
                    Err(err) => result.error = Some(err.to_string()),
                }
            }
            Some(Decision::Port(draft)) => {
                result.action = "port";
                match forge.create_issue(draft) {
                    Ok(created) => {
                        result.issue = Some(IssueJson {
                            number: created.number,
                            url: created.url,
                        });
                        match remove(finding, root, snapshots) {
                            Ok(()) => result.removed = true,
                            Err(err) => {
                                result.error = Some(format!(
                                    "issue {} created, but the comment could not be removed: {err}",
                                    created.number
                                ))
                            }
                        }
                    }
                    Err(err) => result.error = Some(format!("issue not created: {err}")),
                }
            }
        }
        results.push(result);
    }
    results
}

/// Removes the finding's selected comments, verifying the file is unchanged
/// since the scan snapshot.
fn remove(
    finding: &Finding,
    root: &std::path::Path,
    snapshots: &HashMap<PathBuf, FileSnapshot>,
) -> anyhow::Result<()> {
    let path = finding.path();
    let expected = snapshots
        .get(path)
        .with_context(|| format!("no snapshot for {}", path.display()))?;
    let ranges: Vec<std::ops::Range<usize>> = finding.run.comments
        [finding.selection.start..=finding.selection.end]
        .iter()
        .map(|comment| comment.byte_range.clone())
        .collect();
    apply_removal(&root.join(path), &ranges, expected)
        .map(|_| ())
        .map_err(anyhow::Error::from)
}

/// Prints the execution summary. Exits with code 1 when anything failed.
///
/// # Errors
///
/// Returns an error when the output cannot be written.
pub fn print_execution(
    out: &mut dyn Write,
    session: &Session,
    results: &[ExecutionResult],
    json: bool,
) -> anyhow::Result<()> {
    if json {
        serde_json::to_writer_pretty(&mut *out, results)?;
        writeln!(out)?;
        return Ok(());
    }
    let mut ok = true;
    for result in results {
        match (&result.issue, result.removed, &result.error) {
            (Some(issue), removed, None) => {
                writeln!(
                    out,
                    "ok {}/{} ported to #{} ({}) comment {}",
                    result.index + 1,
                    session.len(),
                    issue.number,
                    issue.url,
                    if removed { "removed" } else { "kept" }
                )?;
            }
            (None, true, None) => {
                writeln!(
                    out,
                    "ok {}/{} deleted comment",
                    result.index + 1,
                    session.len()
                )?;
            }
            (None, false, None) => {}
            (_, _, Some(err)) => {
                ok = false;
                writeln!(
                    out,
                    "error {}/{} {}: {}",
                    result.index + 1,
                    session.len(),
                    result.action,
                    err
                )?;
            }
        }
    }
    if ok {
        writeln!(out, "done")?;
    } else {
        writeln!(out, "failed")?;
        std::process::exit(1);
    }
    Ok(())
}

/// Prints the plan without executing anything (`--dry-run`).
fn print_plan(out: &mut dyn Write, session: &Session) -> anyhow::Result<()> {
    let rows: Vec<_> = session
        .items()
        .iter()
        .map(|item| {
            let finding = &item.finding;
            let primary = &finding.run.comments[finding.primary];
            let (action, title) = match &item.decision {
                Some(Decision::Port(draft)) => ("port", draft.title.clone()),
                Some(Decision::Delete) => ("delete", String::new()),
                Some(Decision::Skip) => ("skip", String::new()),
                None => ("undecided", String::new()),
            };
            format!(
                "{:>3}  {:<8} {:<6} {:<30} {}",
                item.index + 1,
                action,
                finding.category,
                primary.path.display().to_string(),
                title
            )
        })
        .collect();
    writeln!(out, "{}", rows.join("\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use todone_core::model::{Comment, CommentRun, Selection};
    use todone_forge::process::{Call, ScriptedRunner};

    fn finding(path: &str, line: usize, text: &str) -> Finding {
        Finding {
            run: CommentRun {
                comments: vec![Comment {
                    path: path.into(),
                    line,
                    end_line: line,
                    column: 0,
                    byte_range: 0..text.len(),
                    text: text.into(),
                    language: "rust".into(),
                }],
            },
            category: "TODO".into(),
            primary: 0,
            selection: Selection::full(1),
        }
    }

    fn session_with(findings: Vec<Finding>) -> Session {
        Session::new(findings, "abc123".into())
    }

    fn snapshots(root: &std::path::Path, session: &Session) -> HashMap<PathBuf, FileSnapshot> {
        capture_snapshots(session, root).unwrap()
    }

    #[test]
    fn execute_ports_then_removes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "// TODO: fix\ncode\n").unwrap();

        let mut session = session_with(vec![finding("src/a.rs", 1, "// TODO: fix")]);
        session.set_decision(0, Decision::Port(auto_draft(&session.findings[0], "abc")));
        let snaps = snapshots(root, &session);

        let runner = ScriptedRunner::new();
        runner.push(
            true,
            r#"{"number": 7, "url": "https://github.com/o/r/issues/7"}"#,
            "",
        );
        let forge =
            todone_forge::forge::GitHubForge::new(Box::new(runner.clone()), Some("o/r".into()));

        let results = execute(&session, &forge, &snaps, root);
        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(result.action, "port");
        assert_eq!(result.issue.as_ref().unwrap().number, 7);
        assert!(result.removed);
        assert!(result.error.is_none());
        assert_eq!(
            std::fs::read_to_string(root.join("src/a.rs")).unwrap(),
            "code\n"
        );

        // The forge saw the issue creation with the draft.
        let calls: Vec<Call> = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args[0], "issue");
    }

    #[test]
    fn execute_issue_failure_keeps_the_comment() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "// TODO: fix\n").unwrap();

        let mut session = session_with(vec![finding("a.rs", 1, "// TODO: fix")]);
        session.set_decision(0, Decision::Port(auto_draft(&session.findings[0], "abc")));
        let snaps = snapshots(root, &session);

        let runner = ScriptedRunner::new();
        runner.push(false, "", "gh: not authenticated");
        let forge =
            todone_forge::forge::GitHubForge::new(Box::new(runner.clone()), Some("o/r".into()));

        let results = execute(&session, &forge, &snaps, root);
        let result = &results[0];
        assert!(result.issue.is_none());
        assert!(!result.removed);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("not authenticated")
        );
        // The comment is still there: nothing was lost.
        assert_eq!(
            std::fs::read_to_string(root.join("a.rs")).unwrap(),
            "// TODO: fix\n"
        );
    }

    #[test]
    fn execute_removal_failure_keeps_the_issue() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "// TODO: fix\n").unwrap();

        let mut session = session_with(vec![finding("a.rs", 1, "// TODO: fix")]);
        session.set_decision(0, Decision::Port(auto_draft(&session.findings[0], "abc")));
        let snaps = snapshots(root, &session);

        let runner = ScriptedRunner::new();
        runner.push(true, r#"{"number": 9, "url": "https://x/9"}"#, "");
        let forge =
            todone_forge::forge::GitHubForge::new(Box::new(runner.clone()), Some("o/r".into()));

        // The file changes after the snapshot: the removal must be refused.
        std::fs::write(root.join("a.rs"), "// TODO: fix\nchanged by the user\n").unwrap();
        let results = execute(&session, &forge, &snaps, root);
        let result = &results[0];
        assert!(result.issue.is_some());
        assert!(!result.removed);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("changed since it was scanned")
        );
    }

    #[test]
    fn execute_deletes_without_issue() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "// TODO: fix\n").unwrap();

        let mut session = session_with(vec![finding("a.rs", 1, "// TODO: fix")]);
        session.set_decision(0, Decision::Delete);
        let snaps = snapshots(root, &session);

        let runner = ScriptedRunner::new();
        let forge =
            todone_forge::forge::GitHubForge::new(Box::new(runner.clone()), Some("o/r".into()));
        let results = execute(&session, &forge, &snaps, root);
        assert_eq!(results[0].action, "delete");
        assert!(results[0].removed);
        assert!(runner.call_count() == 0, "no forge call for deletes");
        assert_eq!(std::fs::read_to_string(root.join("a.rs")).unwrap(), "");
    }

    #[test]
    fn execute_skips_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "// TODO: fix\n").unwrap();

        let mut session = session_with(vec![finding("a.rs", 1, "// TODO: fix")]);
        session.set_decision(0, Decision::Skip);
        let snaps = snapshots(root, &session);

        let runner = ScriptedRunner::new();
        let forge =
            todone_forge::forge::GitHubForge::new(Box::new(runner.clone()), Some("o/r".into()));
        let results = execute(&session, &forge, &snaps, root);
        assert_eq!(results[0].action, "skip");
        assert!(!results[0].removed);
        assert!(results[0].error.is_none());
        assert_eq!(
            std::fs::read_to_string(root.join("a.rs")).unwrap(),
            "// TODO: fix\n"
        );
    }

    #[test]
    fn auto_decide_port_builds_drafts() {
        let mut session = session_with(vec![finding("a.rs", 1, "// TODO: fix this")]);
        auto_decide(&mut session, AutoDecision::Port);
        let Decision::Port(draft) = session.decision(0).unwrap() else {
            panic!("expected port");
        };
        assert_eq!(draft.title, "// TODO: fix this");
        assert!(draft.description.contains("a.rs:1"));
        assert_eq!(draft.commit, "abc123");
    }

    #[test]
    fn mode_confirm_reached_after_all_decided() {
        let mut app = PortApp::new(
            session_with(vec![
                finding("a.rs", 1, "// TODO: x"),
                finding("b.rs", 1, "// TODO: y"),
            ]),
            &crate::run::scan::ScanContext {
                config: todone_core::config::Config::defaults(),
                repo: todone_core::repo::no_repo(std::env::temp_dir()),
            },
        );
        use ratatui::crossterm::event::KeyCode::Char;
        app.handle_key(Char('s'));
        app.handle_key(Char('s'));
        assert_eq!(app.mode, crate::tui::Mode::Confirm);
        assert_eq!(app.handle_key(Char('y')), AppAction::Execute);
    }
}

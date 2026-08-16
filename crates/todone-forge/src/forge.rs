//! Issue-tracking forge backends.
//!
//! [`Forge`] is the seam between the interactive flow and the outside
//! world. v1 ships [`GitHubForge`], which drives the `gh` CLI; more
//! backends (GitLab, Gitea, direct HTTP) can be added behind the same
//! trait without touching the session logic.

use std::path::PathBuf;

use serde::Deserialize;
use thiserror::Error;
use todone_core::config::ForgeConfig;
use todone_core::draft::IssueDraft;

use crate::process::{ProcessOutput, ProcessRunner};

/// A successfully created issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCreated {
    /// The issue number on the forge.
    pub number: u64,
    /// A URL to the issue.
    pub url: String,
}

/// A forge that can create issues from drafts.
pub trait Forge {
    /// Stable backend id (`github`, ...), shown in output and config.
    fn id(&self) -> &'static str;

    /// Creates the issue described by `draft`.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] when the backend cannot run, rejects the
    /// draft, or returns unexpected output.
    fn create_issue(&self, draft: &IssueDraft) -> Result<IssueCreated, ForgeError>;
}

/// Errors produced while interacting with a forge.
#[derive(Debug, Error)]
pub enum ForgeError {
    /// A backend command failed (non-zero exit).
    #[error("command failed: {program} {args}: {stderr}")]
    CommandFailed {
        /// The program that failed.
        program: String,
        /// The arguments it was given.
        args: String,
        /// Its stderr output.
        stderr: String,
    },
    /// The backend's output could not be parsed.
    #[error("failed to parse backend output: {0}")]
    Parse(String),
    /// The repository the backend operates on could not be determined.
    #[error("could not determine the repository: {0}")]
    Repository(String),
    /// The configured forge id is not supported.
    #[error("unsupported forge {0}")]
    Unsupported(String),
    /// A subprocess could not be spawned.
    #[error("failed to run {program}: {source}")]
    Io {
        /// The program that failed to start.
        program: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

fn checked(
    program: &str,
    args: &[&str],
    output: ProcessOutput,
) -> Result<ProcessOutput, ForgeError> {
    if output.success() {
        Ok(output)
    } else {
        Err(ForgeError::CommandFailed {
            program: program.to_string(),
            args: args.join(" "),
            stderr: output.stderr,
        })
    }
}

/// GitHub backend, v1: shells out to the `gh` CLI.
///
/// Requires `gh` on `PATH` and an authenticated session. The repository is
/// either passed explicitly (`owner/name`) or resolved from the repository
/// at `root` via `gh repo view`, so porting a scope that lives in another
/// repository targets that repository.
pub struct GitHubForge {
    runner: Box<dyn ProcessRunner>,
    /// Explicit `owner/name`; `None` resolves from the repository at `root`.
    repo: Option<String>,
    /// The repository root `gh` runs in to resolve the owner/name.
    root: Option<PathBuf>,
    /// Resolved `owner/name`, cached after the first `gh repo view`.
    resolved: std::cell::RefCell<Option<String>>,
}

impl GitHubForge {
    /// Creates a GitHub backend. `repo` overrides repository resolution;
    /// `root` is the directory `gh` queries for the repository.
    pub fn new(
        runner: Box<dyn ProcessRunner>,
        repo: Option<String>,
        root: Option<PathBuf>,
    ) -> Self {
        Self {
            runner,
            repo,
            root,
            resolved: std::cell::RefCell::new(None),
        }
    }

    /// Resolves the `owner/name` of the repository the backend operates on.
    ///
    /// Uses the explicit override when given, otherwise asks `gh` about the
    /// repository at `root` (the process directory when `root` is unset).
    /// The result is cached, so repeated calls do not re-run `gh`.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::CommandFailed`] when `gh` fails and
    /// [`ForgeError::Parse`] when its output is unexpected.
    pub fn resolve_repo(&self) -> Result<String, ForgeError> {
        if let Some(repo) = &self.repo {
            return Ok(repo.clone());
        }
        if let Some(cached) = self.resolved.borrow().as_ref() {
            return Ok(cached.clone());
        }
        let args = ["repo", "view", "--json", "nameWithOwner"];
        let output = self
            .runner
            .run("gh", &args, None, self.root.as_deref())
            .map_err(|source| ForgeError::Io {
                program: "gh".into(),
                source,
            })?;
        let output = checked("gh", &args, output)?;
        let value: serde_json::Value =
            serde_json::from_str(&output.stdout).map_err(|e| ForgeError::Parse(e.to_string()))?;
        let repo = value
            .get("nameWithOwner")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ForgeError::Parse("missing nameWithOwner".into()))?;
        *self.resolved.borrow_mut() = Some(repo.clone());
        Ok(repo)
    }
}

impl Forge for GitHubForge {
    fn id(&self) -> &'static str {
        "github"
    }

    fn create_issue(&self, draft: &IssueDraft) -> Result<IssueCreated, ForgeError> {
        let repo = self.resolve_repo()?;
        let args = [
            "issue",
            "create",
            "--repo",
            &repo,
            "--title",
            &draft.title,
            "--body",
            &draft.description,
            "--json",
            "number,url",
        ];
        let output = self
            .runner
            .run("gh", &args, None, None)
            .map_err(|source| ForgeError::Io {
                program: "gh".into(),
                source,
            })?;
        let output = checked("gh", &args, output)?;
        parse_created(&output.stdout)
    }
}

/// Parses `gh issue create --json number,url` output.
fn parse_created(stdout: &str) -> Result<IssueCreated, ForgeError> {
    #[derive(Deserialize)]
    struct Created {
        number: u64,
        url: String,
    }
    let created: Created =
        serde_json::from_str(stdout).map_err(|e| ForgeError::Parse(e.to_string()))?;
    Ok(IssueCreated {
        number: created.number,
        url: created.url,
    })
}

/// Builds the backend for a [`ForgeConfig`].
///
/// `root` is the repository directory the backend resolves its repository
/// from; `gh` runs there when it needs to.
///
/// # Errors
///
/// Returns [`ForgeError::Unsupported`] for unknown forge ids.
///
/// # Examples
///
/// ```
/// use todone_core::config::ForgeConfig;
/// use todone_forge::{forge, process::ScriptedRunner};
///
/// let config = ForgeConfig { kind: "github".into() };
/// let forge = forge::from_config(&config, Box::new(ScriptedRunner::new()), None).unwrap();
/// assert_eq!(forge.id(), "github");
///
/// let config = ForgeConfig { kind: "gitlab".into() };
/// assert!(forge::from_config(&config, Box::new(ScriptedRunner::new()), None).is_err());
/// ```
pub fn from_config(
    config: &ForgeConfig,
    runner: Box<dyn ProcessRunner>,
    root: Option<PathBuf>,
) -> Result<Box<dyn Forge>, ForgeError> {
    match config.kind.as_str() {
        "github" => Ok(Box::new(GitHubForge::new(runner, None, root))),
        other => Err(ForgeError::Unsupported(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn draft() -> IssueDraft {
        IssueDraft {
            category: "TODO".into(),
            path: "src/lib.rs".into(),
            commit: "abc123".into(),
            title: "Fix it".into(),
            description: "It's broken.".into(),
        }
    }

    fn gh_runner() -> (GitHubForge, crate::process::ScriptedRunner) {
        let runner = crate::process::ScriptedRunner::new();
        let forge = GitHubForge::new(Box::new(runner.clone()), Some("owner/repo".into()), None);
        (forge, runner)
    }

    #[test]
    fn create_issue_builds_the_right_command() {
        let (forge, runner) = gh_runner();
        runner.push(
            true,
            r#"{"number": 42, "url": "https://github.com/owner/repo/issues/42"}"#,
            "",
        );

        let created = forge.create_issue(&draft()).unwrap();
        assert_eq!(created.number, 42);
        assert_eq!(created.url, "https://github.com/owner/repo/issues/42");

        let call = &runner.calls()[0];
        assert_eq!(call.program, "gh");
        assert_eq!(
            call.args,
            [
                "issue",
                "create",
                "--repo",
                "owner/repo",
                "--title",
                "Fix it",
                "--body",
                "It's broken.",
                "--json",
                "number,url",
            ]
        );
    }

    #[test]
    fn create_issue_reports_gh_failures() {
        let (forge, runner) = gh_runner();
        runner.push(false, "", "gh: not logged in");

        let err = forge.create_issue(&draft()).unwrap_err();
        assert!(matches!(err, ForgeError::CommandFailed { .. }));
        assert!(err.to_string().contains("not logged in"));
    }

    #[test]
    fn create_issue_parses_bad_output() {
        let (forge, runner) = gh_runner();
        runner.push(true, "not json", "");

        let err = forge.create_issue(&draft()).unwrap_err();
        assert!(matches!(err, ForgeError::Parse(_)));
    }

    #[test]
    fn resolve_repo_uses_override_without_running_gh() {
        let (forge, runner) = gh_runner();
        assert_eq!(forge.resolve_repo().unwrap(), "owner/repo");
        assert_eq!(runner.call_count(), 0);
    }

    #[test]
    fn resolve_repo_asks_gh_without_override() {
        let runner = crate::process::ScriptedRunner::new();
        runner.push(true, r#"{"nameWithOwner": "other/proj"}"#, "");
        let forge = GitHubForge::new(Box::new(runner.clone()), None, None);

        assert_eq!(forge.resolve_repo().unwrap(), "other/proj");
        let call = &runner.calls()[0];
        assert_eq!(call.args, ["repo", "view", "--json", "nameWithOwner"]);
        // No root given: gh runs in the process's own directory.
        assert_eq!(call.cwd, None);
    }

    #[test]
    fn resolve_repo_runs_gh_in_the_target_root() {
        let root = std::path::PathBuf::from("/tmp/target-repo");
        let runner = crate::process::ScriptedRunner::new();
        runner.push(true, r#"{"nameWithOwner": "owner/repo-b"}"#, "");
        let forge = GitHubForge::new(Box::new(runner.clone()), None, Some(root.clone()));

        assert_eq!(forge.resolve_repo().unwrap(), "owner/repo-b");
        let call = &runner.calls()[0];
        assert_eq!(call.cwd.as_deref(), Some(root.as_path()));
    }

    #[test]
    fn resolve_repo_is_cached() {
        let runner = crate::process::ScriptedRunner::new();
        runner.push(true, r#"{"nameWithOwner": "owner/repo-b"}"#, "");
        let forge = GitHubForge::new(Box::new(runner.clone()), None, None);

        assert_eq!(forge.resolve_repo().unwrap(), "owner/repo-b");
        assert_eq!(forge.resolve_repo().unwrap(), "owner/repo-b");
        assert_eq!(runner.call_count(), 1);
    }

    #[test]
    fn create_issue_resolves_repo_once_from_root() {
        let root = std::path::PathBuf::from("/tmp/target-repo");
        let runner = crate::process::ScriptedRunner::new();
        runner.push(true, r#"{"nameWithOwner": "owner/repo-b"}"#, "");
        runner.push(
            true,
            r#"{"number": 7, "url": "https://github.com/owner/repo-b/issues/7"}"#,
            "",
        );
        let forge = GitHubForge::new(Box::new(runner.clone()), None, Some(root.clone()));

        let created = forge.create_issue(&draft()).unwrap();
        assert_eq!(created.number, 7);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        // The resolution ran in the target root; the issue creation used
        // the resolved --repo from anywhere.
        assert_eq!(calls[0].cwd.as_deref(), Some(root.as_path()));
        assert_eq!(calls[1].cwd, None);
        assert!(calls[1].args.contains(&"--repo".to_string()));
        assert!(calls[1].args.contains(&"owner/repo-b".to_string()));
    }

    #[test]
    fn resolve_repo_reports_bad_gh_output() {
        let runner = crate::process::ScriptedRunner::new();
        runner.push(true, "nope", "");
        let forge = GitHubForge::new(Box::new(runner.clone()), None, None);
        assert!(matches!(
            forge.resolve_repo().unwrap_err(),
            ForgeError::Parse(_)
        ));
    }

    #[test]
    fn resolve_repo_maps_spawn_failures() {
        struct FailingRunner;
        impl crate::process::ProcessRunner for FailingRunner {
            fn run(
                &self,
                _program: &str,
                _args: &[&str],
                _input: Option<&str>,
                _cwd: Option<&Path>,
            ) -> Result<crate::process::ProcessOutput, std::io::Error> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "gh is not installed",
                ))
            }
        }
        let forge = GitHubForge::new(Box::new(FailingRunner), None, None);
        assert!(matches!(
            forge.resolve_repo().unwrap_err(),
            ForgeError::Io { program, .. } if program == "gh"
        ));
    }

    #[test]
    fn forge_id_is_github() {
        let (forge, _) = gh_runner();
        assert_eq!(forge.id(), "github");
    }
}

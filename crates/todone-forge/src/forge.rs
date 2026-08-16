//! Issue-tracking forge backends.
//!
//! [`Forge`] is the seam between the interactive flow and the outside
//! world. v1 ships [`GitHubForge`], which drives the `gh` CLI; more
//! backends (GitLab, Gitea, direct HTTP) can be added behind the same
//! trait without touching the session logic.

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
/// either passed explicitly (`owner/name`) or resolved from the current
/// repository via `gh repo view`.
pub struct GitHubForge {
    runner: Box<dyn ProcessRunner>,
    /// Explicit `owner/name`; `None` resolves from the repository.
    repo: Option<String>,
}

impl GitHubForge {
    /// Creates a GitHub backend. `repo` overrides repository resolution.
    pub fn new(runner: Box<dyn ProcessRunner>, repo: Option<String>) -> Self {
        Self { runner, repo }
    }

    /// Resolves the `owner/name` of the repository the backend operates on.
    ///
    /// Uses the explicit override when given, otherwise asks `gh` about the
    /// repository in the current directory.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::CommandFailed`] when `gh` fails and
    /// [`ForgeError::Parse`] when its output is unexpected.
    pub fn resolve_repo(&self) -> Result<String, ForgeError> {
        if let Some(repo) = &self.repo {
            return Ok(repo.clone());
        }
        let args = ["repo", "view", "--json", "nameWithOwner"];
        let output = self
            .runner
            .run("gh", &args, None)
            .map_err(|source| ForgeError::Io {
                program: "gh".into(),
                source,
            })?;
        let output = checked("gh", &args, output)?;
        let value: serde_json::Value =
            serde_json::from_str(&output.stdout).map_err(|e| ForgeError::Parse(e.to_string()))?;
        value
            .get("nameWithOwner")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ForgeError::Parse("missing nameWithOwner".into()))
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
            .run("gh", &args, None)
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
/// let forge = forge::from_config(&config, Box::new(ScriptedRunner::new())).unwrap();
/// assert_eq!(forge.id(), "github");
///
/// let config = ForgeConfig { kind: "gitlab".into() };
/// assert!(forge::from_config(&config, Box::new(ScriptedRunner::new())).is_err());
/// ```
pub fn from_config(
    config: &ForgeConfig,
    runner: Box<dyn ProcessRunner>,
) -> Result<Box<dyn Forge>, ForgeError> {
    match config.kind.as_str() {
        "github" => Ok(Box::new(GitHubForge::new(runner, None))),
        other => Err(ForgeError::Unsupported(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let forge = GitHubForge::new(Box::new(runner.clone()), Some("owner/repo".into()));
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
        let forge = GitHubForge::new(Box::new(runner.clone()), None);

        assert_eq!(forge.resolve_repo().unwrap(), "other/proj");
        let call = &runner.calls()[0];
        assert_eq!(call.args, ["repo", "view", "--json", "nameWithOwner"]);
    }

    #[test]
    fn resolve_repo_reports_bad_gh_output() {
        let runner = crate::process::ScriptedRunner::new();
        runner.push(true, "nope", "");
        let forge = GitHubForge::new(Box::new(runner.clone()), None);
        assert!(matches!(
            forge.resolve_repo().unwrap_err(),
            ForgeError::Parse(_)
        ));
    }

    #[test]
    fn forge_id_is_github() {
        let (forge, _) = gh_runner();
        assert_eq!(forge.id(), "github");
    }
}

//! Git repository discovery and scope resolution.
//!
//! v1 shells out to the `git` binary (a documented command dependency); the
//! functions here are the single seam where that happens, so a future
//! version can switch to an embedded implementation.
//!
//! Scope resolution decides *which* repository a scan targets when paths
//! are given on the command line: the first scope path's repository wins,
//! with fallbacks walking back toward the current directory and warnings
//! describing what was chosen.

use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

/// Information about the repository a scan runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// Absolute path of the repository root.
    pub root: PathBuf,
    /// Current `HEAD` commit hash, when the repo has commits.
    pub commit: Option<String>,
    /// Whether `root` is an actual git repository (as opposed to a
    /// fallback directory with no version control).
    pub is_repo: bool,
    /// The `origin` remote URL, when the repo has one.
    pub remote: Option<String>,
}

/// Discovers the git repository containing `start`.
///
/// Returns `Ok(None)` when `start` is not inside a git work tree or when
/// `git` is unavailable; the caller then falls back to treating `start` as
/// the root with no commit.
pub fn discover_repo(start: &Path) -> std::io::Result<Option<RepoInfo>> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let commit = current_commit(&root).ok().flatten();
    let remote = remote_url(&root).ok().flatten();
    Ok(Some(RepoInfo {
        root,
        commit,
        is_repo: true,
        remote,
    }))
}

/// A fallback repository info for paths outside any git repository.
pub fn no_repo(root: PathBuf) -> RepoInfo {
    RepoInfo {
        root,
        commit: None,
        is_repo: false,
        remote: None,
    }
}

fn current_commit(root: &Path) -> std::io::Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

fn remote_url(root: &Path) -> std::io::Result<Option<String>> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

/// The outcome of resolving the scan scope: the target repository plus the
/// absolute scope targets and any warnings about what was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScope {
    /// The repository the scan targets.
    pub repo: RepoInfo,
    /// The scope targets, absolute and canonicalized.
    pub targets: Vec<PathBuf>,
    /// Human-readable warnings (multi-repo scope, non-git fallback, ...).
    pub warnings: Vec<String>,
}

/// Errors produced while resolving the scan scope.
#[derive(Debug, Error)]
pub enum RepoError {
    /// A scope path does not exist.
    #[error("path does not exist: {0}")]
    MissingPath(PathBuf),
    /// A git or filesystem operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Resolves the repository a scan should target.
///
/// - With an empty scope, the repository containing `start` is used.
/// - Otherwise every scope path is canonicalized; the *first* path decides
///   the repository: its own deepest git root, or — walking back toward
///   `start` — the repository containing `start`, or — failing both — the
///   path itself as a fallback root with no version control. Paths that
///   resolve to a different repository than the chosen one produce a
///   warning; the first repository still wins.
///
/// # Examples
///
/// ```
/// use todone_core::repo::resolve_scope;
///
/// // An empty scope resolves to the start directory and no targets.
/// let scope = resolve_scope(std::env::temp_dir().as_path(), &[]).unwrap();
/// assert!(scope.targets.is_empty());
/// ```
pub fn resolve_scope(start: &Path, scope: &[PathBuf]) -> Result<ResolvedScope, RepoError> {
    if scope.is_empty() {
        let repo = discover_repo(start)?.unwrap_or_else(|| no_repo(start.to_path_buf()));
        let mut warnings = Vec::new();
        if !repo.is_repo {
            warnings.push(format!(
                "{} is not inside a git repository; using {} as the base \
                 directory (no forge sink)",
                start.display(),
                repo.root.display()
            ));
        }
        return Ok(ResolvedScope {
            repo,
            targets: Vec::new(),
            warnings,
        });
    }

    let mut targets = Vec::with_capacity(scope.len());
    for path in scope {
        let absolute = start.join(path);
        let canonical = absolute
            .canonicalize()
            .map_err(|_| RepoError::MissingPath(absolute))?;
        targets.push(canonical);
    }

    let mut warnings = Vec::new();
    let (repo, fallback_base) = resolve_first_target(start, &targets[0], &mut warnings)?;

    for target in &targets[1..] {
        let other = discover_repo(target_dir(target))?.map(|r| r.root);
        if let Some(other) = other.filter(|other| *other != repo.root) {
            warnings.push(format!(
                "scope spans multiple repositories; using {} and ignoring {}",
                repo.root.display(),
                other.display()
            ));
        }
    }

    if !repo.is_repo {
        warnings.push(format!(
            "{} is not inside a git repository; using {} as the base \
             directory (no forge sink)",
            fallback_base.display(),
            repo.root.display()
        ));
    }

    // Rebase the scope targets relative to the chosen root (the root
    // itself becomes ".").
    let rebased: Vec<PathBuf> = targets
        .iter()
        .map(|t| {
            let rel = t.strip_prefix(&repo.root).unwrap_or(t);
            if rel.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                rel.to_path_buf()
            }
        })
        .collect();

    Ok(ResolvedScope {
        repo,
        targets: rebased,
        warnings,
    })
}

/// Resolves the repository for the first scope target.
///
/// Preference order: the target's own deepest git root, then — walking back
/// toward `start` — the repository containing `start`, then the target
/// itself as a fallback root.
fn resolve_first_target(
    start: &Path,
    first: &Path,
    warnings: &mut Vec<String>,
) -> Result<(RepoInfo, PathBuf), RepoError> {
    let base = target_dir(first);
    if let Some(repo) = discover_repo(base)? {
        return Ok((repo, base.to_path_buf()));
    }
    if let Some(repo) = discover_repo(start)? {
        warnings.push(format!(
            "{} is not inside a git repository; using the repository at {}",
            first.display(),
            repo.root.display()
        ));
        return Ok((repo, base.to_path_buf()));
    }
    Ok((no_repo(base.to_path_buf()), base.to_path_buf()))
}

/// The directory to run git discovery in for a scope target: the target
/// itself, or its parent when it is a file.
fn target_dir(target: &Path) -> &Path {
    if target.is_file() {
        target.parent().unwrap_or(target)
    } else {
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    /// Creates a git repo at `dir` with one committed file.
    fn make_repo(dir: &Path) {
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::fs::write(dir.join("a.rs"), "// TODO: x\n").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "x",
            ])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn discovers_root_commit_and_remote_of_a_repo() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_repo(root);

        let repo = discover_repo(root).unwrap().unwrap();
        assert_eq!(repo.root, root.canonicalize().unwrap());
        assert!(repo.is_repo);
        assert!(repo.commit.as_deref().is_some_and(|c| c.len() == 40));
        assert_eq!(repo.remote, None);

        let sub = root.join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        let repo = discover_repo(&sub).unwrap().unwrap();
        assert_eq!(repo.root, root.canonicalize().unwrap());
    }

    #[test]
    fn no_repo_yields_none_and_fallback_marks_it() {
        let dir = tempfile::tempdir().unwrap();
        let result = discover_repo(dir.path()).unwrap();
        assert!(result.is_none());
        let fallback = no_repo(dir.path().to_path_buf());
        assert!(!fallback.is_repo);
        assert_eq!(fallback.commit, None);
    }

    #[test]
    fn empty_scope_uses_start() {
        let dir = tempfile::tempdir().unwrap();
        let scope = resolve_scope(dir.path(), &[]).unwrap();
        assert!(!scope.repo.is_repo);
        assert_eq!(scope.repo.root, dir.path());
        assert!(scope.targets.is_empty());
        assert!(scope.warnings.iter().any(|w| w.contains("no forge sink")));
    }

    #[test]
    fn scope_inside_a_repo_resolves_to_it() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_repo(root);
        std::fs::create_dir_all(root.join("src")).unwrap();

        let scope = resolve_scope(root, &[PathBuf::from("src")]).unwrap();
        assert!(scope.repo.is_repo);
        assert_eq!(scope.repo.root, root.canonicalize().unwrap());
        assert_eq!(scope.targets, vec![PathBuf::from("src")]);
        assert!(scope.warnings.is_empty());
    }

    #[test]
    fn scope_in_another_repo_rebases_to_it() {
        if !git_available() {
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        let a = parent.path().join("repo-a");
        let b = parent.path().join("repo-b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        make_repo(&a);
        make_repo(&b);
        std::fs::write(b.join("main.rs"), "// TODO: x\n").unwrap();

        // From repo-a, scope into repo-b: repo-b wins and the path rebases.
        let scope = resolve_scope(&a, &[PathBuf::from("../repo-b")]).unwrap();
        assert_eq!(scope.repo.root, b.canonicalize().unwrap());
        assert_eq!(scope.targets, vec![PathBuf::from(".")]);
        assert!(scope.warnings.is_empty());
    }

    #[test]
    fn file_scope_uses_its_parent_for_discovery() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_repo(root);
        let file = root.join("a.rs");

        let scope = resolve_scope(root, std::slice::from_ref(&file)).unwrap();
        assert!(scope.repo.is_repo);
        assert_eq!(scope.repo.root, root.canonicalize().unwrap());
        assert_eq!(scope.targets, vec![PathBuf::from("a.rs")]);
    }

    #[test]
    fn missing_scope_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_scope(dir.path(), &[PathBuf::from("nope")]).unwrap_err();
        assert!(matches!(err, RepoError::MissingPath(_)));
    }

    #[test]
    fn multiple_repos_warn_and_first_wins() {
        if !git_available() {
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        let a = parent.path().join("repo-a");
        let b = parent.path().join("repo-b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        make_repo(&a);
        make_repo(&b);

        let scope = resolve_scope(&a, &[PathBuf::from("."), PathBuf::from("../repo-b")]).unwrap();
        assert_eq!(scope.repo.root, a.canonicalize().unwrap());
        assert_eq!(scope.targets[0], PathBuf::from("."));
        // Targets outside the chosen root stay absolute.
        assert_eq!(scope.targets[1], b.canonicalize().unwrap());
        assert!(
            scope
                .warnings
                .iter()
                .any(|w| w.contains("spans multiple repositories"))
        );
    }

    #[test]
    fn non_git_scope_falls_back_to_start_repo() {
        if !git_available() {
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        let a = parent.path().join("repo-a");
        let other = parent.path().join("plain-dir");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        make_repo(&a);
        std::fs::write(other.join("f.rs"), "// TODO: x\n").unwrap();

        let scope = resolve_scope(&a, &[PathBuf::from("../plain-dir")]).unwrap();
        assert_eq!(scope.repo.root, a.canonicalize().unwrap());
        assert!(scope.repo.is_repo);
        assert!(
            scope
                .warnings
                .iter()
                .any(|w| w.contains("not inside a git repository"))
        );
    }

    #[test]
    fn non_git_scope_without_start_repo_warns_and_uses_base() {
        let parent = tempfile::tempdir().unwrap();
        let other = parent.path().join("plain-dir");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("f.rs"), "// TODO: x\n").unwrap();

        let scope = resolve_scope(parent.path(), &[PathBuf::from("plain-dir")]).unwrap();
        assert!(!scope.repo.is_repo);
        assert_eq!(scope.repo.root, other.canonicalize().unwrap());
        assert!(scope.warnings.iter().any(|w| w.contains("no forge sink")));
    }
}

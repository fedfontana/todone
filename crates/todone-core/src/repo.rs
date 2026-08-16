//! Git repository discovery.
//!
//! v1 shells out to the `git` binary (a documented command dependency); the
//! functions here are the single seam where that happens, so a future
//! version can switch to an embedded implementation.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Information about the repository a scan runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// Absolute path of the repository root.
    pub root: PathBuf,
    /// Current `HEAD` commit hash, when the repo has commits.
    pub commit: Option<String>,
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
    Ok(Some(RepoInfo { root, commit }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_root_and_commit_of_a_repo() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(root)
            .output()
            .unwrap();
        std::fs::write(root.join("a.rs"), "// TODO: x\n").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(root)
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
            .current_dir(root)
            .output()
            .unwrap();

        let repo = discover_repo(root).unwrap().unwrap();
        assert_eq!(repo.root, root.canonicalize().unwrap());
        assert!(repo.commit.as_deref().is_some_and(|c| c.len() == 40));

        let sub = root.join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        let repo = discover_repo(&sub).unwrap().unwrap();
        assert_eq!(repo.root, root.canonicalize().unwrap());
    }

    #[test]
    fn no_repo_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let result = discover_repo(dir.path()).unwrap();
        assert!(result.is_none());
    }
}

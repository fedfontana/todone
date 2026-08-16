//! End-to-end tests for the `todone` binary.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

/// Creates a small fixture repository with comments in two languages.
fn repo_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    // TODO: fix this\n    // FIXME: and this\n}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("lib.py"),
        "def f():\n    # TODO: python todo\n    pass\n",
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    dir
}

fn todone() -> Command {
    Command::cargo_bin("todone").unwrap()
}

#[test]
fn scan_prints_findings_with_context() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("src/main.rs:2: TODO"))
        .stdout(predicate::str::contains("lib.py:2: TODO"))
        .stdout(predicate::str::contains("│     // TODO: fix this"))
        // The FIXME on the next line is part of the same run and shows up
        // in the context rather than as its own finding.
        .stdout(predicate::str::contains("src/main.rs:3: FIXME").not())
        .stdout(predicate::str::contains("// FIXME: and this"));
}

#[test]
fn scan_json_output_is_parseable_and_complete() {
    let dir = repo_fixture();
    let output = todone()
        .current_dir(dir.path())
        .args(["scan", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0]["category"], "TODO");
    assert_eq!(findings[0]["path"], "lib.py");
    assert_eq!(findings[1]["path"], "src/main.rs");
    assert_eq!(findings[1]["line"], 2);
    assert_eq!(findings[1]["comments"].as_array().unwrap().len(), 2);
    assert!(report["repo"]["root"].is_string());
    assert!(report["stats"]["files"].is_u64());
}

#[test]
fn scan_respects_path_scope() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["scan", "src"])
        .assert()
        .success()
        .stdout(predicate::str::contains("src/main.rs:2: TODO"))
        .stdout(predicate::str::contains("lib.py").not());
}

#[test]
fn scan_no_matches_is_silent_success() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    todone()
        .current_dir(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn scan_rejects_invalid_config() {
    let dir = repo_fixture();
    fs::write(
        dir.path().join("todone.toml"),
        "[scan.match]\ncategories = []\n",
    )
    .unwrap();
    todone()
        .current_dir(dir.path())
        .arg("scan")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "at least one category is required",
        ));
}

#[test]
fn scan_with_custom_pattern_flags() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["scan", "--pattern", "FIXME"])
        .assert()
        .success()
        .stdout(predicate::str::contains("src/main.rs:3: FIXME"))
        .stdout(predicate::str::contains(": TODO").not());
}

#[test]
fn scan_anchored_default_ignores_doc_mentions() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("a.rs"),
        "/// Triage TODO comments: scan, review, and port them.\n// TODO: the real one\nfn main() {}\n",
    )
    .unwrap();
    todone()
        .current_dir(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("a.rs:2: TODO"))
        .stdout(predicate::str::contains("a.rs:1: TODO").not());
}

#[test]
fn scan_with_custom_match_pattern() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("a.rs"),
        "/// Triage TODO comments: scan.\n// TODO: the real one\n// TODO without colon\nfn main() {}\n",
    )
    .unwrap();
    todone()
        .current_dir(dir.path())
        .args(["scan", "--match-pattern", "^{comment}{marker}:{content}"])
        .assert()
        .success()
        .stdout(predicate::str::contains("a.rs:2: TODO"))
        .stdout(predicate::str::contains("a.rs:1: TODO").not())
        .stdout(predicate::str::contains("a.rs:3: TODO").not());
}

#[test]
fn scan_rejects_invalid_match_pattern() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["scan", "--match-pattern", "{comment}{marker}:{nope}"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown placeholder"));
}

#[test]
fn config_prints_sample_and_effective() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["config", "--sample"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[scan.match]"));

    todone()
        .current_dir(dir.path())
        .arg("config")
        .assert()
        .success()
        .stdout(predicate::str::contains("[scan.match]"))
        .stdout(predicate::str::contains("kind = \"github\""));
}

#[test]
fn port_auto_skip_dry_run_prints_the_plan() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "skip", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skip"))
        .stdout(predicate::str::contains("src/main.rs"));
    // Nothing was touched.
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        "fn main() {\n    // TODO: fix this\n    // FIXME: and this\n}\n"
    );
}

#[test]
fn port_auto_delete_removes_comments() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "delete"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted comment"))
        .stdout(predicate::str::contains("done"));
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        "fn main() {\n}\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("lib.py")).unwrap(),
        "def f():\n    pass\n"
    );
}

#[test]
fn port_auto_delete_json() {
    let dir = repo_fixture();
    let output = todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "delete", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let results: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let results = results.as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r["action"] == "delete" && r["removed"] == true)
    );
}

#[test]
fn port_no_findings_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "delete"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no marker comments found"));
    assert_eq!(
        fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        "fn main() {}\n"
    );
}

#[test]
fn unknown_subcommand_fails() {
    todone()
        .arg("frobnicate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

/// Creates a bare git repo at `dir` with a committed file.
fn make_git_repo(dir: &std::path::Path, file: &str, content: &str) {
    std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir)
        .output()
        .unwrap();
    let path = dir.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(dir)
        .output()
        .unwrap();
}

#[test]
fn port_scope_in_another_repo_targets_that_repo() {
    let parent = tempfile::tempdir().unwrap();
    let a = parent.path().join("repo-a");
    let b = parent.path().join("repo-b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    make_git_repo(&a, "a.rs", "fn a() {}\n");
    make_git_repo(&b, "main.rs", "fn main() {\n    // TODO: fix\n}\n");

    todone()
        .current_dir(&a)
        .args(["port", "--auto", "delete", "../repo-b"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted comment"))
        // The context banner names repo-b as the target.
        .stderr(predicate::str::contains(format!(
            "repo {}",
            b.canonicalize().unwrap().display()
        )));

    // The comment was removed from repo-b; repo-a is untouched.
    assert_eq!(
        fs::read_to_string(b.join("main.rs")).unwrap(),
        "fn main() {\n}\n"
    );
    assert_eq!(fs::read_to_string(a.join("a.rs")).unwrap(), "fn a() {}\n");
}

#[test]
fn scan_scope_in_another_repo_reports_that_repo() {
    let parent = tempfile::tempdir().unwrap();
    let a = parent.path().join("repo-a");
    let b = parent.path().join("repo-b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    make_git_repo(&a, "a.rs", "fn a() {}\n");
    make_git_repo(&b, "main.rs", "fn main() {\n    // TODO: fix\n}\n");
    let b_root = b.canonicalize().unwrap();

    let output = todone()
        .current_dir(&a)
        .args(["scan", "--json", "../repo-b"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        report["repo"]["root"],
        b_root.to_string_lossy().into_owned()
    );
    assert_eq!(report["repo"]["is_repo"], true);
    // The commit is repo-b's HEAD.
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&b)
        .output()
        .unwrap();
    let commit = String::from_utf8_lossy(&commit.stdout).trim().to_string();
    assert_eq!(report["repo"]["commit"], commit);
    assert_eq!(report["findings"][0]["path"], "main.rs");
}

#[test]
fn scope_in_another_repo_uses_that_repos_config() {
    let parent = tempfile::tempdir().unwrap();
    let a = parent.path().join("repo-a");
    let b = parent.path().join("repo-b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    make_git_repo(&a, "a.rs", "fn a() {}\n");
    make_git_repo(&b, "main.rs", "fn main() {\n    // TODO: fix\n}\n");
    // repo-b configures a different category: the TODO must not match.
    fs::write(
        b.join("todone.toml"),
        "[scan.match]\ncategories = [\"PERF\"]\n",
    )
    .unwrap();

    todone()
        .current_dir(&a)
        .args(["scan", "../repo-b"])
        .assert()
        .success()
        .stdout(predicate::str::contains(": TODO").not());
}

#[test]
fn scope_spanning_two_repos_warns_and_first_wins() {
    let parent = tempfile::tempdir().unwrap();
    let a = parent.path().join("repo-a");
    let b = parent.path().join("repo-b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    make_git_repo(&a, "a.rs", "// TODO: in a\n");
    make_git_repo(&b, "b.rs", "// TODO: in b\n");

    todone()
        .current_dir(&a)
        .args(["scan", ".", "../repo-b"])
        .assert()
        .success()
        .stdout(predicate::str::contains("a.rs:1: TODO"))
        .stderr(predicate::str::contains("spans multiple repositories"));
}

#[test]
fn port_outside_a_repo_errors_scan_works() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f.rs"), "// TODO: x\n").unwrap();

    todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "delete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not in a git repository"));

    todone()
        .current_dir(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("f.rs:1: TODO"))
        .stderr(predicate::str::contains("no forge sink"));
}

#[test]
fn missing_scope_path_errors() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "delete", "does-not-exist"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path does not exist"));
}

#[test]
fn port_with_unknown_forge_fails() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "delete", "--forge", "gitlab"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown forge"));
}

#[test]
fn scan_rejects_unknown_placeholder_in_config_pattern() {
    let dir = repo_fixture();
    fs::write(
        dir.path().join("todone.toml"),
        "[scan.match]\npattern = \"{comment}{marker}{nope}\"\n",
    )
    .unwrap();
    todone()
        .current_dir(dir.path())
        .arg("scan")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown placeholder"));
}

#[test]
fn scan_rejects_empty_category() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["scan", "--pattern", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("categories must not be empty"));
}

#[test]
fn port_rejects_invalid_auto_value() {
    let dir = repo_fixture();
    todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "frobnicate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn port_skips_non_utf8_files() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    fs::write(dir.path().join("bin.rs"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
    todone()
        .current_dir(dir.path())
        .args(["port", "--auto", "delete"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no marker comments found"));
}

#[test]
fn completions_generate_for_shells() {
    let output = todone()
        .arg("completions")
        .arg("bash")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    assert!(!text.is_empty());
    assert!(text.contains("todone"));

    let output = todone()
        .arg("completions")
        .arg("zsh")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("#compdef"));
}

#[test]
fn completions_reject_unknown_shells() {
    todone()
        .args(["completions", "frobnicate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

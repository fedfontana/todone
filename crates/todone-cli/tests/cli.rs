//! End-to-end tests for the `todone` binary.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

/// Creates a small fixture repository with comments in two languages.
fn repo_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
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
        .args(["scan", "--pattern", "FIXME", "--no-space"])
        .assert()
        .success()
        .stdout(predicate::str::contains("src/main.rs:3: FIXME"))
        .stdout(predicate::str::contains(": TODO").not());
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

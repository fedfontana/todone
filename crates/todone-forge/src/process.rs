//! A thin, testable process-execution layer.
//!
//! The forge backends shell out to CLIs (`gh` for v1). Every call goes
//! through [`ProcessRunner`], which tests replace with a scripted fake; the
//! day a backend grows up (e.g. direct HTTP for GitHub), it is the only
//! seam that disappears.

use std::fmt;
use std::path::{Path, PathBuf};

/// The outcome of running a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    /// Whether the process exited successfully.
    pub success: bool,
    /// The exit code, when the process was not killed by a signal.
    pub code: Option<i32>,
    /// Captured stdout (lossy UTF-8).
    pub stdout: String,
    /// Captured stderr (lossy UTF-8).
    pub stderr: String,
}

impl ProcessOutput {
    /// Whether the process exited with code 0.
    pub fn success(&self) -> bool {
        self.success
    }
}

/// Something that can run a program, capturing its output.
///
/// `cwd` selects the working directory of the process, so commands can be
/// run against a specific repository.
///
/// # Examples
///
/// ```
/// use todone_forge::process::{ProcessOutput, ProcessRunner};
///
/// struct Noop;
/// impl ProcessRunner for Noop {
///     fn run(&self, _program: &str, _args: &[&str], _input: Option<&str>, _cwd: Option<&std::path::Path>) -> Result<ProcessOutput, std::io::Error> {
///         Ok(ProcessOutput { success: true, code: Some(0), stdout: String::new(), stderr: String::new() })
///     }
/// }
/// ```
pub trait ProcessRunner {
    /// Runs `program` with `args`, writing `input` to its stdin when given,
    /// from working directory `cwd` (the process's own directory when
    /// `None`).
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<ProcessOutput, std::io::Error>;
}

/// The production runner: executes real processes via `std::process`.
#[derive(Debug, Default)]
pub struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<ProcessOutput, std::io::Error> {
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn()?;
        if let Some(input) = input {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(input.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        Ok(ProcessOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// A recorded invocation, for assertions in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The program that was invoked.
    pub program: String,
    /// The arguments passed.
    pub args: Vec<String>,
    /// The stdin payload, when any.
    pub stdin: Option<String>,
    /// The working directory the process ran in.
    pub cwd: Option<PathBuf>,
}

impl fmt::Display for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.program, self.args.join(" "))
    }
}

/// A test double: serves canned responses in FIFO order and records every
/// call.
///
/// Clones share the same queue and call log, so a test can hand a clone to
/// the code under test and assert on the other. Intended for unit tests and
/// for driving the CLI's port flow without touching a real forge.
#[derive(Debug, Clone, Default)]
pub struct ScriptedRunner {
    inner: std::rc::Rc<std::cell::RefCell<ScriptedRunnerInner>>,
}

#[derive(Debug, Default)]
struct ScriptedRunnerInner {
    responses: std::collections::VecDeque<ProcessOutput>,
    calls: Vec<Call>,
}

impl ScriptedRunner {
    /// Creates an empty scripted runner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues one response; calls beyond the queue fail.
    pub fn push(&self, success: bool, stdout: &str, stderr: &str) {
        self.inner.borrow_mut().responses.push_back(ProcessOutput {
            success,
            code: Some(if success { 0 } else { 1 }),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        });
    }

    /// How many invocations have been recorded.
    pub fn call_count(&self) -> usize {
        self.inner.borrow().calls.len()
    }

    /// A snapshot of every invocation, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.inner.borrow().calls.clone()
    }
}

impl ProcessRunner for ScriptedRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        input: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<ProcessOutput, std::io::Error> {
        let mut inner = self.inner.borrow_mut();
        inner.calls.push(Call {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            stdin: input.map(str::to_string),
            cwd: cwd.map(Path::to_path_buf),
        });
        Ok(inner
            .responses
            .pop_front()
            .unwrap_or_else(|| ProcessOutput {
                success: false,
                code: Some(1),
                stdout: String::new(),
                stderr: "no scripted response".into(),
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_runner_runs_echo() {
        let runner = SystemProcessRunner;
        let out = runner.run("echo", &["hello"], None, None).unwrap();
        assert!(out.success());
        assert_eq!(out.stdout.trim(), "hello");
    }

    #[test]
    fn system_runner_reports_failures() {
        let runner = SystemProcessRunner;
        let out = runner.run("sh", &["-c", "exit 3"], None, None).unwrap();
        assert!(!out.success());
        assert_eq!(out.code, Some(3));
    }

    #[test]
    fn scripted_runner_records_and_serves() {
        let runner = ScriptedRunner::new();
        runner.push(true, "out1", "");
        runner.push(false, "", "boom");
        let out = runner.run("gh", &["--version"], Some("in"), None).unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, "out1");
        let out = runner.run("gh", &["--version"], None, None).unwrap();
        assert!(!out.success());
        assert_eq!(out.stderr, "boom");

        assert_eq!(runner.call_count(), 2);
        let calls = runner.calls();
        assert_eq!(calls[0].program, "gh");
        assert_eq!(calls[0].stdin.as_deref(), Some("in"));
        assert_eq!(calls[0].args, vec!["--version"]);
    }

    #[test]
    fn scripted_runner_fails_on_unscripted_calls() {
        let runner = ScriptedRunner::new();
        let out = runner.run("gh", &[], None, None).unwrap();
        assert!(!out.success());
        assert!(out.stderr.contains("no scripted response"));
    }

    #[test]
    fn system_runner_reports_spawn_failures() {
        let runner = SystemProcessRunner;
        let err = runner
            .run("definitely-not-a-real-program-todone", &[], None, None)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}

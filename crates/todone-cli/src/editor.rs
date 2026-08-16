//! Editor sessions: drafting issues and read-only code views.
//!
//! The editor is resolved from configuration or `$EDITOR`, defaulting to
//! `nvim`. Drafts are edited in a temporary file; read-only views open the
//! real file in vim-family editors with `-R`, or a throwaway copy for
//! editors that cannot be started read-only.

use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use todone_core::config::EditorConfig;

/// Resolves and runs editor sessions.
#[derive(Debug, Clone)]
pub struct Editor {
    /// The raw command from config or `$EDITOR` (whitespace-split).
    pub command: Vec<String>,
}

impl Editor {
    /// Resolves the editor command from `config` or `$EDITOR`.
    pub fn resolve(config: &EditorConfig) -> Self {
        let raw = config
            .command
            .clone()
            .or_else(|| std::env::var("EDITOR").ok())
            .unwrap_or_else(|| "nvim".to_string());
        Self {
            command: raw.split_whitespace().map(str::to_string).collect(),
        }
    }

    /// Whether the editor is vim-family and supports `-R`/`+line`.
    fn supports_vim_flags(&self) -> bool {
        self.command
            .first()
            .map(|p| {
                let base = Path::new(p)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                base.contains("vim") || base.contains("nvim")
            })
            .unwrap_or(false)
    }

    /// Runs the editor on `path` (which must exist) and waits for it.
    ///
    /// # Errors
    ///
    /// Returns an error if the editor cannot be spawned or the file does
    /// not exist.
    pub fn edit_file(&self, path: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(path.exists(), "{} does not exist", path.display());
        self.run(&[path.to_string_lossy().as_ref()])
    }

    /// Opens `rel` (relative to `root`) for read-only inspection, at `line`
    /// when the editor supports it.
    ///
    /// vim-family editors open the real file with `-R`; other editors get a
    /// throwaway copy that keeps the original extension, so syntax
    /// highlighting still applies.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the editor cannot be
    /// spawned.
    pub fn view_readonly(&self, root: &Path, rel: &Path, line: usize) -> anyhow::Result<()> {
        let path = root.join(rel);
        let display = path.to_string_lossy();
        if self.supports_vim_flags() {
            self.run(&["-R", &format!("+{line}"), display.as_ref()])
        } else {
            let suffix = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let mut tmp = tempfile::Builder::new()
                .prefix("todone-view-")
                .suffix(&suffix)
                .tempfile()
                .context("cannot create a temporary copy for the read-only view")?;
            let content = std::fs::read(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            std::io::Write::write_all(&mut tmp, &content)
                .context("cannot write the temporary copy")?;
            let tmp_path = tmp.path().to_path_buf();
            let result = self.run(&[tmp_path.to_string_lossy().as_ref()]);
            // The temp file is deleted on drop.
            drop(tmp);
            result
        }
    }

    /// Runs the editor with `args`, inheriting the terminal.
    fn run(&self, args: &[&str]) -> anyhow::Result<()> {
        let (program, fixed) = self
            .command
            .split_first()
            .expect("editor command is non-empty");
        // A freshly written script can transiently be "text file busy";
        // retry briefly instead of failing the session.
        for attempt in 0..5 {
            match Command::new(program).args(fixed).args(args).status() {
                Ok(status) => {
                    anyhow::ensure!(status.success(), "editor `{program}` exited with {status}");
                    return Ok(());
                }
                Err(err) if attempt < 4 && err.raw_os_error() == Some(libc_etxtbsy()) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(err) => {
                    return Err(anyhow::Error::new(err)
                        .context(format!("failed to run editor `{program}`")));
                }
            }
        }
        Ok(())
    }
}

/// The `ETXTBSY` errno on Linux (26), used to retry freshly written
/// executables. Non-Linux platforms get a value that never matches.
fn libc_etxtbsy() -> i32 {
    if cfg!(target_os = "linux") { 26 } else { -1 }
}

/// The outcome of a draft editing session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftOutcome {
    /// The user saved a valid draft.
    Drafted(todone_core::draft::IssueDraft),
    /// The user quit without changes (or left the placeholder title).
    Aborted,
}

/// Opens the draft template for `draft` in the editor and decodes the
/// result.
///
/// # Errors
///
/// Returns an error when the temporary file cannot be created or read.
pub fn edit_draft(
    editor: &Editor,
    draft: &todone_core::draft::IssueDraft,
    snippet: &todone_core::draft::ContextSnippet,
) -> anyhow::Result<DraftOutcome> {
    let rendered = todone_core::draft::render_draft(draft, snippet);
    // The .md suffix makes vim-family editors set ft=markdown, and keeps
    // the file readable in any other editor.
    let mut tmp = tempfile::Builder::new()
        .prefix("todone-draft-")
        .suffix(".md")
        .tempfile()
        .context("cannot create draft file")?;
    std::io::Write::write_all(&mut tmp, rendered.as_bytes()).context("cannot write draft file")?;
    let path = tmp.path().to_path_buf();

    match editor.edit_file(&path) {
        Ok(()) => {
            let content = std::fs::read_to_string(&path).context("cannot read draft file")?;
            if content == rendered {
                return Ok(DraftOutcome::Aborted);
            }
            match todone_core::draft::parse_draft(&content) {
                Ok(draft) => Ok(DraftOutcome::Drafted(draft)),
                Err(todone_core::draft::DraftError::PlaceholderTitle) => Ok(DraftOutcome::Aborted),
                Err(err) => anyhow::bail!(
                    "the draft template was modified in a way todone cannot decode: {err}"
                ),
            }
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use todone_core::draft::{ContextSnippet, IssueDraft};

    /// Tests that mutate `$EDITOR` must hold this lock, since they run in
    /// parallel.
    static EDITOR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn make_executable(path: &Path) {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn draft() -> IssueDraft {
        IssueDraft {
            category: "TODO".into(),
            path: "src/lib.rs".into(),
            commit: "abc".into(),
            title: "Fix it".into(),
            description: "Broken.".into(),
        }
    }

    fn snippet() -> ContextSnippet {
        ContextSnippet {
            language: "rust".into(),
            text: "fn main() {}\n".into(),
        }
    }

    #[test]
    fn editor_resolves_from_editor_env() {
        let _guard = EDITOR_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("EDITOR", "code --wait") };
        let config = EditorConfig { command: None };
        let editor = Editor::resolve(&config);
        assert_eq!(editor.command, vec!["code", "--wait"]);
        assert!(!editor.supports_vim_flags());
        unsafe { std::env::remove_var("EDITOR") };
    }

    #[test]
    fn config_overrides_editor_env() {
        let _guard = EDITOR_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("EDITOR", "emacs") };
        let config = EditorConfig {
            command: Some("nvim".into()),
        };
        let editor = Editor::resolve(&config);
        assert_eq!(editor.command, vec!["nvim"]);
        assert!(editor.supports_vim_flags());
        unsafe { std::env::remove_var("EDITOR") };
    }

    #[test]
    fn defaults_to_nvim() {
        let _guard = EDITOR_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("EDITOR") };
        let editor = Editor::resolve(&EditorConfig { command: None });
        assert_eq!(editor.command, vec!["nvim"]);
        assert!(editor.supports_vim_flags());
    }

    #[test]
    fn edit_file_missing_path_errors() {
        let editor = Editor::resolve(&EditorConfig { command: None });
        let err = editor.edit_file(Path::new("/nonexistent/xyz")).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn view_readonly_passes_vim_flags() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "code\n").unwrap();
        // We cannot run a real editor in tests; assert flag construction via
        // a fake vim-like script.
        let fake = dir.path().join("fakevim");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FAKEVIM_OUT\"\n",
        )
        .unwrap();
        make_executable(&fake);
        let editor = Editor {
            command: vec![fake.to_string_lossy().into_owned()],
        };
        assert!(editor.supports_vim_flags());
        unsafe {
            std::env::set_var(
                "FAKEVIM_OUT",
                dir.path().join("args").to_string_lossy().as_ref(),
            )
        };
        editor
            .view_readonly(dir.path(), Path::new("a.rs"), 3)
            .unwrap();
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert!(args.contains("-R"));
        assert!(args.contains("+3"));
        assert!(args.contains("a.rs"));
        unsafe { std::env::remove_var("FAKEVIM_OUT") };
    }

    #[test]
    fn view_readonly_copy_keeps_the_original_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.py"), "code\n").unwrap();
        // A non-vim editor: the view goes through a temp copy.
        let fake = dir.path().join("fakeeditor");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FAKE_EDITOR_OUT\"\n",
        )
        .unwrap();
        make_executable(&fake);
        let editor = Editor {
            command: vec![fake.to_string_lossy().into_owned()],
        };
        assert!(!editor.supports_vim_flags());
        unsafe {
            std::env::set_var(
                "FAKE_EDITOR_OUT",
                dir.path().join("args2").to_string_lossy().as_ref(),
            )
        };
        editor
            .view_readonly(dir.path(), Path::new("lib.py"), 1)
            .unwrap();
        let args = std::fs::read_to_string(dir.path().join("args2")).unwrap();
        let copy = args.trim();
        assert!(copy.starts_with("/tmp/"), "temp copy expected, got {copy}");
        assert!(copy.ends_with(".py"), "extension must survive, got {copy}");
        unsafe { std::env::remove_var("FAKE_EDITOR_OUT") };
    }

    #[test]
    fn draft_temp_file_is_markdown() {
        let dir = tempfile::tempdir().unwrap();
        // A fake editor that echoes its argument and edits nothing.
        let script = dir.path().join("echoedit.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$ECHO_OUT\"\n",
        )
        .unwrap();
        make_executable(&script);
        let editor = Editor {
            command: vec![script.to_string_lossy().into_owned()],
        };
        unsafe {
            std::env::set_var(
                "ECHO_OUT",
                dir.path().join("path.txt").to_string_lossy().as_ref(),
            )
        };
        // Content unchanged -> aborted, but the path was recorded first.
        let outcome = edit_draft(&editor, &draft(), &snippet()).unwrap();
        assert_eq!(outcome, DraftOutcome::Aborted);
        let path = std::fs::read_to_string(dir.path().join("path.txt")).unwrap();
        assert!(
            path.trim().ends_with(".md"),
            "draft must be markdown, got {path}"
        );
        unsafe { std::env::remove_var("ECHO_OUT") };
    }

    #[test]
    fn edit_draft_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        // A fake editor that writes a valid draft by filling the template.
        let script = dir.path().join("fakeedit.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             sed -i 's|# Title: .*|# Title: Ported from review|' \"$1\"\n\
             sed -i 's|<write the issue description here>|It crashes.|' \"$1\"\n",
        )
        .unwrap();
        make_executable(&script);
        let editor = Editor {
            command: vec![script.to_string_lossy().into_owned()],
        };
        let outcome = edit_draft(&editor, &draft(), &snippet()).unwrap();
        let DraftOutcome::Drafted(parsed) = outcome else {
            panic!("expected a draft");
        };
        assert_eq!(parsed.title, "Ported from review");
        assert_eq!(parsed.category, "TODO");
    }

    #[test]
    fn edit_draft_unchanged_is_aborted() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("noop.sh");
        std::fs::write(&script, "#!/bin/sh\ncat \"$1\" >/dev/null\n").unwrap();
        make_executable(&script);
        let editor = Editor {
            command: vec![script.to_string_lossy().into_owned()],
        };
        let outcome = edit_draft(&editor, &draft(), &snippet()).unwrap();
        assert_eq!(outcome, DraftOutcome::Aborted);
    }

    #[test]
    fn edit_draft_reports_editor_failures() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fail.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 4\n").unwrap();
        make_executable(&script);
        let editor = Editor {
            command: vec![script.to_string_lossy().into_owned()],
        };
        let err = edit_draft(&editor, &draft(), &snippet()).unwrap_err();
        assert!(err.to_string().contains("exited with"), "{err}");
    }

    #[test]
    fn edit_draft_reports_corrupted_templates() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("corrupt.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'this is no template\\n' > \"$1\"\n",
        )
        .unwrap();
        make_executable(&script);
        let editor = Editor {
            command: vec![script.to_string_lossy().into_owned()],
        };
        let err = edit_draft(&editor, &draft(), &snippet()).unwrap_err();
        assert!(err.to_string().contains("cannot decode"), "{err}");
    }
}

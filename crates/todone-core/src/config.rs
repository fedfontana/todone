//! Layered configuration: defaults, a user config file, a repository config
//! file, and CLI overrides.
//!
//! Precedence, lowest to highest:
//!
//! 1. built-in defaults,
//! 2. `$TODONE_CONFIG` or `$XDG_CONFIG_HOME/todone/config.toml` (or
//!    `~/.config/todone/config.toml`),
//! 3. `<repo root>/todone.toml` (committed, shared with the team),
//! 4. command-line flags ([`CliOverrides`]).
//!
//! A layer only overrides the keys it sets; everything else falls through to
//! the next lower layer.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::matcher::MatchConfig;
use crate::scan::ScanOptions;

/// Fully resolved configuration after merging all layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Config {
    /// Scanning behavior.
    pub scan: ScanConfig,
    /// Context lines shown around a finding.
    pub context: ContextConfig,
    /// Issue-tracking backend.
    pub forge: ForgeConfig,
    /// Editor used for draft and read-only sessions.
    pub editor: EditorConfig,
}

/// Scanning behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanConfig {
    /// Marker category matching.
    #[serde(rename = "match")]
    pub match_config: MatchConfig,
    /// Default scope: paths (relative to the repo root) to scan when none
    /// are given on the command line.
    pub paths: Vec<PathBuf>,
    /// Glob patterns (relative to the repo root) to exclude.
    pub exclude: Vec<String>,
    /// Files larger than this many bytes are skipped.
    pub max_file_bytes: usize,
}

/// Context lines around a finding, like `grep -A`/`-B`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ContextConfig {
    /// Lines of context before the finding.
    pub before: usize,
    /// Lines of context after the finding.
    pub after: usize,
    /// Word-wrap long context lines in the TUI (toggleable with `w`).
    pub wrap: bool,
}

/// Issue-tracking backend selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeConfig {
    /// Backend id (`github` for v1).
    pub kind: String,
}

/// Editor behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditorConfig {
    /// Editor command; `None` falls back to `$EDITOR`.
    pub command: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scan: ScanConfig {
                match_config: MatchConfig::default(),
                paths: Vec::new(),
                exclude: Vec::new(),
                max_file_bytes: 10 * 1024 * 1024,
            },
            context: ContextConfig {
                before: 3,
                after: 3,
                wrap: true,
            },
            forge: ForgeConfig {
                kind: "github".into(),
            },
            editor: EditorConfig { command: None },
        }
    }
}

impl Config {
    /// The built-in default configuration.
    pub fn defaults() -> Self {
        Self::default()
    }

    /// Loads the configuration for `repo_root`, merging defaults, user
    /// config, repo config, and CLI overrides.
    ///
    /// `user_config` is the resolved user-config path ([`user_config_path`]
    /// or a `$TODONE_CONFIG` override); pass `None` to skip the user layer
    /// entirely. Missing config files are not an error.
    ///
    /// # Errors
    ///
    /// Returns an error when a config file exists but cannot be read or
    /// parsed, or when the merged configuration is invalid.
    pub fn load(
        repo_root: &Path,
        user_config: Option<&Path>,
        overrides: &CliOverrides,
    ) -> Result<Self, ConfigError> {
        let mut config = Self::defaults();

        if let Some(path) = user_config
            && path.exists()
        {
            config.merge_file(path)?;
        }

        let repo_path = repo_root.join("todone.toml");
        if repo_path.exists() {
            config.merge_file(&repo_path)?;
        }

        config.apply_overrides(overrides);
        config.validate()?;
        Ok(config)
    }

    /// Merges the partial configuration from `path` into `self`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or is not valid TOML.
    pub fn merge_file(&mut self, path: &Path) -> Result<(), ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let file: ConfigFile = toml::from_str(&content).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        self.merge_file_struct(file);
        Ok(())
    }

    fn merge_file_struct(&mut self, file: ConfigFile) {
        if let Some(scan) = file.scan {
            if let Some(match_config) = scan.match_config {
                self.scan.match_config = match_config;
            }
            if let Some(paths) = scan.paths {
                self.scan.paths = paths;
            }
            if let Some(exclude) = scan.exclude {
                self.scan.exclude = exclude;
            }
            if let Some(max_file_bytes) = scan.max_file_bytes {
                self.scan.max_file_bytes = max_file_bytes;
            }
        }
        if let Some(context) = file.context {
            if let Some(before) = context.before {
                self.context.before = before;
            }
            if let Some(after) = context.after {
                self.context.after = after;
            }
        }
        if let Some(forge) = file.forge
            && let Some(kind) = forge.kind
        {
            self.forge.kind = kind;
        }
        if let Some(editor) = file.editor
            && let Some(command) = editor.command
        {
            self.editor.command = Some(command);
        }
    }

    /// Applies command-line overrides on top of everything else.
    pub fn apply_overrides(&mut self, overrides: &CliOverrides) {
        if let Some(categories) = &overrides.categories {
            self.scan.match_config.categories = categories.clone();
        }
        if let Some(case_sensitive) = overrides.case_sensitive {
            self.scan.match_config.case_sensitive = case_sensitive;
        }
        if let Some(pattern) = &overrides.match_pattern {
            self.scan.match_config.pattern = Some(pattern.clone());
        }
        if let Some(paths) = &overrides.paths {
            self.scan.paths = paths.clone();
        }
        if let Some(exclude) = &overrides.exclude {
            self.scan.exclude = exclude.clone();
        }
        if let Some(max_file_bytes) = overrides.max_file_bytes {
            self.scan.max_file_bytes = max_file_bytes;
        }
        if let Some(before) = overrides.context_before {
            self.context.before = before;
        }
        if let Some(after) = overrides.context_after {
            self.context.after = after;
        }
        if let Some(wrap) = overrides.wrap {
            self.context.wrap = wrap;
        }
        if let Some(kind) = &overrides.forge {
            self.forge.kind = kind.clone();
        }
        if let Some(command) = &overrides.editor {
            self.editor.command = Some(command.clone());
        }
    }

    /// Checks the configuration for internal consistency.
    ///
    /// # Errors
    ///
    /// Returns an error when the match configuration cannot be compiled
    /// (e.g. an empty category list) or when the forge is unknown.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.scan.match_config.compile()?;
        if self.forge.kind != "github" {
            return Err(ConfigError::UnknownForge(self.forge.kind.clone()));
        }
        Ok(())
    }

    /// Builds the [`ScanOptions`] that implement the scan-related parts of
    /// this configuration.
    pub fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            paths: self.scan.paths.clone(),
            exclude: self.scan.exclude.clone(),
            match_config: self.scan.match_config.clone(),
            max_file_bytes: self.scan.max_file_bytes,
        }
    }

    /// Renders the configuration as TOML, for `todone config` output.
    ///
    /// # Examples
    ///
    /// ```
    /// use todone_core::config::Config;
    ///
    /// let toml = Config::defaults().to_toml();
    /// assert!(toml.contains("[context]"));
    /// ```
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("config always serializes")
    }

    /// A commented sample `todone.toml` to get users started.
    pub fn sample() -> String {
        SAMPLE_CONFIG.to_string()
    }
}

/// The sample configuration shipped with `todone config sample`.
const SAMPLE_CONFIG: &str = r#"# todone configuration. Place this file at the
# repository root to share it with the team, or in
# ~/.config/todone/config.toml for personal settings.
# A file given by $TODONE_CONFIG overrides both.

[scan.match]
# Marker categories to look for, in priority order.
categories = ["TODO", "FIXME"]
# Match categories case-insensitively ("todo" matches "TODO").
case_sensitive = false

# A custom comment pattern (regex-crate syntax) with placeholders:
#   {comment}  the comment marker (a run of non-word characters: //, ///,
#              #, /*, " * " decorations, ...)
#   {marker}   any configured category (captured)
#   {content}  the rest of the line (captured)
# Everything else is matched verbatim. Without a pattern, a built-in rule
# requires the marker to be the first content token of a comment line, so
# doc comments that merely mention a marker ("/// Triage TODO comments")
# are not reported. Anchor custom patterns with ^ to keep that behavior.
# pattern = "^{comment}{marker}:{content}"

# Default scope when no paths are given on the command line.
# paths = ["src"]

# Glob patterns to exclude, relative to the repo root.
# exclude = ["vendor/**"]

# Skip files larger than this many bytes.
# max_file_bytes = 10485760

[context]
# Context lines around each finding, like grep -A/-B.
before = 3
after = 3
# Word-wrap long context lines in the interactive review (toggle with `w`).
wrap = true

[forge]
# Issue-tracking backend: "github" (v1, via the gh CLI).
kind = "github"

[editor]
# Editor used for issue drafts and read-only views.
# Falls back to $EDITOR when unset.
# command = "nvim"
"#;

/// Errors produced while loading or validating configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A config file could not be read.
    #[error("failed to read config {path}: {source}")]
    Io {
        /// The config file path.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// A config file is not valid TOML.
    #[error("failed to parse config {path}: {source}")]
    Parse {
        /// The config file path.
        path: PathBuf,
        /// The TOML error.
        source: toml::de::Error,
    },
    /// The match configuration is invalid.
    #[error(transparent)]
    MatchConfig(#[from] crate::matcher::MatchConfigError),
    /// The configured forge is not supported.
    #[error("unknown forge {0}; supported: github")]
    UnknownForge(String),
}

/// Partial configuration as written in one file. Every key is optional so a
/// file only overrides what it sets.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    /// `[scan]` section.
    pub scan: Option<ScanFile>,
    /// `[context]` section.
    pub context: Option<ContextFile>,
    /// `[forge]` section.
    pub forge: Option<ForgeFile>,
    /// `[editor]` section.
    pub editor: Option<EditorFile>,
}

/// `[scan]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ScanFile {
    /// `[scan.match]` section.
    #[serde(rename = "match")]
    pub match_config: Option<MatchConfig>,
    /// Default scope paths.
    pub paths: Option<Vec<PathBuf>>,
    /// Exclude globs.
    pub exclude: Option<Vec<String>>,
    /// Size cap for parsed files.
    pub max_file_bytes: Option<usize>,
}

/// `[context]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContextFile {
    /// Context lines before a finding.
    pub before: Option<usize>,
    /// Context lines after a finding.
    pub after: Option<usize>,
}

/// `[forge]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ForgeFile {
    /// Backend id.
    pub kind: Option<String>,
}

/// `[editor]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EditorFile {
    /// Editor command.
    pub command: Option<String>,
}

/// Command-line overrides with the same precedence as the CLI flags.
#[derive(Debug, Default)]
pub struct CliOverrides {
    /// `--pattern` category list.
    pub categories: Option<Vec<String>>,
    /// `--case-sensitive` / `--no-case-sensitive`.
    pub case_sensitive: Option<bool>,
    /// `--match-pattern` custom comment pattern.
    pub match_pattern: Option<String>,
    /// Positional scope paths.
    pub paths: Option<Vec<PathBuf>>,
    /// `--exclude` patterns.
    pub exclude: Option<Vec<String>>,
    /// `--max-file-bytes`.
    pub max_file_bytes: Option<usize>,
    /// `-A`.
    pub context_before: Option<usize>,
    /// `-B`.
    pub context_after: Option<usize>,
    /// `--wrap` / `--no-wrap`.
    pub wrap: Option<bool>,
    /// `--forge`.
    pub forge: Option<String>,
    /// `--editor`.
    pub editor: Option<String>,
}

/// Resolves the user config path: `$TODONE_CONFIG`, then
/// `$XDG_CONFIG_HOME/todone/config.toml`, then `~/.config/todone/config.toml`.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TODONE_CONFIG") {
        return Some(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("todone").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn defaults_are_sane() {
        let config = Config::defaults();
        assert_eq!(config.scan.match_config.categories, vec!["TODO", "FIXME"]);
        assert_eq!(config.context.before, 3);
        assert_eq!(config.context.after, 3);
        assert_eq!(config.forge.kind, "github");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn user_config_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cfg = write_config(
            root,
            "config.toml",
            "[scan.match]\ncategories = [\"PERF\"]\n[context]\nbefore = 1\n",
        );
        let config = Config::load(root, Some(&cfg), &CliOverrides::default()).unwrap();
        assert_eq!(config.scan.match_config.categories, vec!["PERF"]);
        assert_eq!(config.context.before, 1);
        assert_eq!(config.context.after, 3); // untouched
    }

    #[test]
    fn repo_config_wins_over_user_config() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let user = write_config(root, "user.toml", "[context]\nbefore = 1\nafter = 1\n");
        write_config(root, "todone.toml", "[context]\nbefore = 9\n");
        let config = Config::load(root, Some(&user), &CliOverrides::default()).unwrap();
        assert_eq!(config.context.before, 9);
        assert_eq!(config.context.after, 1);
    }

    #[test]
    fn cli_overrides_win() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_config(root, "todone.toml", "[context]\nbefore = 9\n");
        let overrides = CliOverrides {
            context_before: Some(2),
            categories: Some(vec!["HACK".into()]),
            ..Default::default()
        };
        let config = Config::load(root, None, &overrides).unwrap();
        assert_eq!(config.context.before, 2);
        assert_eq!(config.scan.match_config.categories, vec!["HACK"]);
    }

    #[test]
    fn missing_files_are_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path(), None, &CliOverrides::default()).unwrap();
        assert_eq!(config, Config::defaults());
    }

    #[test]
    fn invalid_toml_is_reported_with_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cfg = write_config(root, "bad.toml", "not = [valid\n");
        let err = Config::load(root, Some(&cfg), &CliOverrides::default()).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn unknown_forge_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_config(root, "todone.toml", "[forge]\nkind = \"gitlab\"\n");
        let err = Config::load(root, None, &CliOverrides::default()).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownForge(_)));
    }

    #[test]
    fn empty_categories_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_config(root, "todone.toml", "[scan.match]\ncategories = []\n");
        let err = Config::load(root, None, &CliOverrides::default()).unwrap_err();
        assert!(matches!(err, ConfigError::MatchConfig(_)));
    }

    #[test]
    fn partial_match_section_keeps_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_config(root, "todone.toml", "[scan.match]\ncase_sensitive = true\n");
        let config = Config::load(root, None, &CliOverrides::default()).unwrap();
        assert!(config.scan.match_config.case_sensitive);
        assert_eq!(
            config.scan.match_config.categories,
            MatchConfig::default().categories
        );
        assert_eq!(config.scan.match_config.pattern, None);
    }

    #[test]
    fn to_toml_and_sample_round_trip() {
        let config = Config::defaults();
        let rendered = config.to_toml();
        let parsed: ConfigFile = toml::from_str(&rendered).unwrap();
        let mut merged = Config::defaults();
        merged.merge_file_struct(parsed);
        assert_eq!(merged, config);
        assert!(Config::sample().contains("[scan.match]"));
    }

    #[test]
    fn scan_options_reflect_config() {
        let config = Config::defaults();
        let options = config.scan_options();
        assert_eq!(options.match_config, config.scan.match_config);
        assert_eq!(options.max_file_bytes, config.scan.max_file_bytes);
    }
}

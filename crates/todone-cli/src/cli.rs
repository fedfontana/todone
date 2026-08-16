//! Command-line surface (clap).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Triage TODO comments: scan, review, and port them to your forge.
#[derive(Debug, Parser)]
#[command(name = "todone", version, about, propagate_version = true)]
pub struct Cli {
    /// Emit machine-readable JSON output instead of human text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Force colored output even when not on a TTY.
    #[arg(long, global = true)]
    pub color: bool,
    /// Disable colored output.
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scan for marker comments and print them with context.
    Scan(ScanArgs),
    /// Interactively review findings, port them to issues, and remove them.
    Port(PortArgs),
    /// Show the effective configuration or a sample.
    Config(ConfigArgs),
}

/// Shared options for scanning behavior.
#[derive(Debug, Args)]
pub struct CommonScanArgs {
    /// Context lines after each finding (grep -A).
    #[arg(short = 'A', long)]
    pub after: Option<usize>,
    /// Context lines before each finding (grep -B).
    #[arg(short = 'B', long)]
    pub before: Option<usize>,
    /// Comma-separated marker categories, e.g. `TODO,FIXME,PERF`.
    #[arg(long)]
    pub pattern: Option<String>,
    /// Match categories case-sensitively.
    #[arg(long, conflicts_with = "no_case_sensitive")]
    pub case_sensitive: bool,
    /// Match categories case-insensitively (default).
    #[arg(long, conflicts_with = "case_sensitive")]
    pub no_case_sensitive: bool,
    /// Require whitespace before the category (default).
    #[arg(long, conflicts_with = "no_space")]
    pub require_space: bool,
    /// Allow categories without a preceding space, e.g. `//TODO`.
    #[arg(long, conflicts_with = "require_space")]
    pub no_space: bool,
    /// Require a colon after the category (`// TODO: x`).
    #[arg(long, conflicts_with = "no_colon")]
    pub require_colon: bool,
    /// Do not require a colon after the category (default).
    #[arg(long, conflicts_with = "require_colon")]
    pub no_colon: bool,
    /// Glob patterns to exclude, relative to the repo root.
    #[arg(long)]
    pub exclude: Vec<String>,
    /// Skip files larger than this many bytes.
    #[arg(long)]
    pub max_file_bytes: Option<usize>,
    /// Files or directories to scan; empty means the whole repo.
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    #[command(flatten)]
    pub common: CommonScanArgs,
}

#[derive(Debug, Args)]
pub struct PortArgs {
    #[command(flatten)]
    pub common: CommonScanArgs,
    /// Print the plan without creating issues or editing files.
    #[arg(long)]
    pub dry_run: bool,
    /// Decide for every finding without interaction.
    #[arg(long, value_enum)]
    pub auto: Option<AutoDecision>,
    /// Skip the confirmation screen.
    #[arg(long)]
    pub yes: bool,
    /// The forge backend to use (overrides configuration).
    #[arg(long)]
    pub forge: Option<String>,
    /// Editor command for drafts and read-only views.
    #[arg(long)]
    pub editor: Option<String>,
}

/// Decision applied to every finding in `--auto` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AutoDecision {
    /// Leave every comment in place.
    Skip,
    /// Remove every comment without creating an issue.
    Delete,
    /// Create an issue with the comment text as the draft, then remove it.
    Port,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Print the sample todone.toml instead of the effective configuration.
    #[arg(long)]
    pub sample: bool,
}

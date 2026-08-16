//! Execution of the `completions` subcommand: shell completion scripts.

use std::io::Write;

use clap::CommandFactory;

use crate::cli::{Cli, CompletionsArgs};

/// Generates the completion script for the requested shell to `out`.
///
/// # Errors
///
/// Returns an error when the output cannot be written.
pub fn run_completions(out: &mut dyn Write, args: &CompletionsArgs) -> anyhow::Result<()> {
    let mut command = Cli::command();
    clap_complete::generate(args.shell, &mut command, "todone", out);
    Ok(())
}

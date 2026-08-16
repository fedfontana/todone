//! Execution of the `port` subcommand (interactive review and issue
//! creation).

use std::io::Write;

use anyhow::bail;

use crate::cli::PortArgs;
use crate::run::scan::ScanContext;

/// Runs the interactive port flow.
///
/// # Errors
///
/// This stub is replaced by the full interactive flow; for now it always
/// reports that the command is not implemented.
pub fn run_port(
    _out: &mut dyn Write,
    _context: &ScanContext,
    _args: &PortArgs,
    _json: bool,
    _color: bool,
) -> anyhow::Result<()> {
    bail!("`todone port` is not implemented yet")
}

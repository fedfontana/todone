//! Execution of the `config` subcommand.

use std::io::Write;

use anyhow::Context as _;
use todone_core::config::{CliOverrides, Config};

use crate::cli::ConfigArgs;

/// Runs the config command, printing either the sample or the effective
/// configuration.
pub fn run_config(out: &mut dyn Write, args: &ConfigArgs) -> anyhow::Result<()> {
    if args.sample {
        write!(out, "{}", Config::sample())?;
        return Ok(());
    }
    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let repo = todone_core::repo::discover_repo(&cwd)?.unwrap_or(todone_core::repo::RepoInfo {
        root: cwd,
        commit: None,
    });
    let user_config = todone_core::config::user_config_path();
    let config = Config::load(&repo.root, user_config.as_deref(), &CliOverrides::default())?;
    write!(out, "{}", config.to_toml())?;
    Ok(())
}

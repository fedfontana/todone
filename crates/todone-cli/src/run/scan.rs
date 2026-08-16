//! Execution of the `scan` subcommand.

use std::io::Write;

use anyhow::Context as _;
use todone_core::config::{CliOverrides, Config};
use todone_core::repo::RepoInfo;
use todone_core::scan::Scanner;

use crate::cli::CommonScanArgs;
use crate::context::extract_context;
use crate::highlight::HighlightEngine;
use crate::render::render_context;
use crate::report::{FindingReport, RepoReport, ScanReport};

/// Converts the shared scan flags into config overrides.
pub fn overrides(args: &CommonScanArgs) -> CliOverrides {
    CliOverrides {
        categories: args
            .pattern
            .as_deref()
            .map(|p| p.split(',').map(str::trim).map(str::to_string).collect()),
        case_sensitive: flag(args.case_sensitive, args.no_case_sensitive),
        require_space_before: flag(args.require_space, args.no_space),
        require_colon: flag(args.require_colon, args.no_colon),
        paths: (!args.paths.is_empty()).then(|| args.paths.clone()),
        exclude: (!args.exclude.is_empty()).then(|| args.exclude.clone()),
        max_file_bytes: args.max_file_bytes,
        context_before: args.before,
        context_after: args.after,
        ..Default::default()
    }
}

fn flag(yes: bool, no: bool) -> Option<bool> {
    if yes {
        Some(true)
    } else if no {
        Some(false)
    } else {
        None
    }
}

/// The loaded configuration plus the resolved repository root.
pub struct ScanContext {
    /// Effective configuration.
    pub config: Config,
    /// Repository info (root + commit).
    pub repo: RepoInfo,
}

/// Resolves the repository root and loads the configuration.
pub fn load_context(
    args: &CommonScanArgs,
    forge: Option<&str>,
    editor: Option<&str>,
) -> anyhow::Result<ScanContext> {
    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let repo = todone_core::repo::discover_repo(&cwd)?.unwrap_or(RepoInfo {
        root: cwd,
        commit: None,
    });

    let mut overrides = overrides(args);
    overrides.forge = forge.map(str::to_string);
    overrides.editor = editor.map(str::to_string);

    let user_config = todone_core::config::user_config_path();
    let config = Config::load(&repo.root, user_config.as_deref(), &overrides)
        .context("invalid configuration")?;
    Ok(ScanContext { config, repo })
}

/// Runs a scan and prints the report to `out`.
pub fn run_scan(
    out: &mut dyn Write,
    context: &ScanContext,
    json: bool,
    color: bool,
) -> anyhow::Result<()> {
    let scanner = Scanner::new(context.config.scan_options())?;
    let result = scanner.scan(&context.repo.root)?;

    if json {
        let report = build_report(&result, context);
        serde_json::to_writer_pretty(&mut *out, &report)?;
        writeln!(out)?;
        return Ok(());
    }

    let mut engine = HighlightEngine::new();
    let mut cache: std::collections::HashMap<
        std::path::PathBuf,
        Vec<crate::highlight::HighlightSpan>,
    > = std::collections::HashMap::new();

    for finding in &result.findings {
        let path = context.repo.root.join(finding.path());
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", finding.path().display()))?;
        let primary = &finding.run.comments[finding.primary];
        let spans = if color {
            cache
                .entry(finding.path().to_path_buf())
                .or_insert_with(|| {
                    engine
                        .highlight(&primary.language, &source)
                        .unwrap_or_default()
                })
                .clone()
        } else {
            Vec::new()
        };
        let ctx = extract_context(
            &source,
            finding,
            context.config.context.before,
            context.config.context.after,
        );

        writeln!(
            out,
            "{}",
            crate::render::render_finding_header(
                &finding.path().display().to_string(),
                finding.line(),
                &finding.category,
                color
            )
        )?;
        write!(out, "{}", render_context(&ctx, &spans, color))?;
        writeln!(out)?;
    }

    Ok(())
}

/// Builds the JSON report from a scan result.
pub fn build_report(result: &todone_core::scan::ScanResult, context: &ScanContext) -> ScanReport {
    let mut findings = Vec::new();
    for finding in &result.findings {
        let Ok(source) = std::fs::read_to_string(context.repo.root.join(finding.path())) else {
            continue;
        };
        let ctx = extract_context(
            &source,
            finding,
            context.config.context.before,
            context.config.context.after,
        );
        findings.push(FindingReport::new(finding, &ctx));
    }
    ScanReport {
        repo: RepoReport {
            root: context.repo.root.clone(),
            commit: context.repo.commit.clone(),
        },
        findings,
        stats: result.stats,
    }
}

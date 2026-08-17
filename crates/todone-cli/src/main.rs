//! todone: triage TODO comments and port them to your forge.

mod cli;
mod context;
mod editor;
mod highlight;
mod render;
mod report;
mod run;
mod tui;

use std::io::IsTerminal;

use anyhow::Context as _;
use clap::Parser;

use crate::cli::{Cli, Command};

// TODO: should really change the template for the issue file: first h1 is the title of the issue, everything else is the content. Might even make the title part of the frontmatter, and have the description be everything after it. Might also change the frontmatter to include a comment explaining what to do? Might also Move the content that is auto included to a comment that is not included in the final issue? Maybe configurable
// TODO: currently if the edit breaks the format the whole app quits, let's just void the change, go back to the tui in the same spot and notify the user
// TODO: move some info from the top bar to the bottom bar
// TODO: add some color and nicer in the final page
// TODO: streaming parsing (we lose the total number at start, but can start faster. Maybe move
// parsing to other thread so it can go on while the user looks at the first part?)
// TODO: change `o view` in bottom bar to `o open` and don't promise it to be readonly, since we
// already have the movable preview
// TODO: mouse && scroll support. Currently after using the mouse everything feels off and sluggish
// TODO: react to file changes. Must notify the user if a previously selected TODO is removed/moved via external file change
// TODO: give option to consider the content right after `// TODO:` as the title, and the lines contigous to it (but still in the same comment) as the initial content of the issue
// TODO: move to `turdo` package
// TODO: better help (floating) menu
// TODO: progress floaging page with navigation
// FIXME: this should not be recognized as todo
// ```sh
// │ > 251 │ /// "// TODO: fix\n",                                                                                                                                                                                                                               │
// │ > 252 │ /// ).unwrap();                                                                                                                                                                                                                                     │
// │ > 253 │ /// assert_eq!(comments.len(), 1);                                                                                                                                                                                                                  │
// │ > 254 │ /// assert_eq!(comments[0].line, 1);                                                                                                                                                                                                                │
// │ > 255 │ /// ```
// ```
// // FIXME: also seems like we lose indentation in comments
// ```
// │   242 │ /// ```                                                                                                                                                                                                                                             │
// │   243 │ /// use std::path::Path;                                                                                                                                                                                                                            │
// │   244 │ /// use todone_core::language;                                                                                                                                                                                                                      │
// │   245 │ /// use todone_core::scan::parse_comment_nodes;                                                                                                                                                                                                     │
// │   246 │ ///                                                                                                                                                                                                                                                 │
// │   247 │ /// let lang = language::by_extension(Path::new("a.rs")).unwrap();                                                                                                                                                                                  │
// │   248 │ /// let comments = parse_comment_nodes(                                                                                                                                                                                                             │
// │   249 │ /// Path::new("a.rs").into(),                                                                                                                                                                                                                       │
// │   250 │ /// lang,                                                                                                                                                                                                                                           │
// │ > 251 │ /// "// TODO: fix\n",                                                                                                                                                                                                                               │
// │ > 252 │ /// ).unwrap();                                                                                                                                                                                                                                     │
// │ > 253 │ /// assert_eq!(comments.len(), 1);                                                                                                                                                                                                                  │
// │ > 254 │ /// assert_eq!(comments[0].line, 1);                                                                                                                                                                                                                │
// │ > 255 │ /// ```
// ```

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let color = resolve_color(cli.color, cli.no_color, std::io::stdout().is_terminal());

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    match run(cli, &mut out, color) {
        Ok(()) => Ok(()),
        // Piping into `head` and friends is not an error.
        Err(err) if is_broken_pipe(&err) => Ok(()),
        Err(err) => Err(err),
    }
}

fn run(cli: Cli, out: &mut dyn std::io::Write, color: bool) -> anyhow::Result<()> {
    let mut err = std::io::stderr().lock();
    match cli.command {
        Command::Scan(args) => {
            let context = run::scan::load_context(&args.common, None, None, &mut err)
                .context("failed to set up the scan")?;
            run::scan::run_scan(out, &context, cli.json, color)
        }
        Command::Port(args) => {
            let context = run::scan::load_context(
                &args.common,
                args.forge.as_deref(),
                args.editor.as_deref(),
                &mut err,
            )
            .context("failed to set up the port session")?;
            run::port::run_port(out, &context, &args, cli.json, color)
        }
        Command::Config(args) => run::config::run_config(out, &args),
        Command::Completions(args) => run::completions::run_completions(out, &args),
    }
}

/// Whether an error chain contains an I/O broken-pipe error.
fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
            || (cause
                .downcast_ref::<serde_json::Error>()
                .is_some_and(|e| e.is_io())
                && cause.to_string().contains("Broken pipe"))
    })
}

/// Decides whether output is colored: explicit flags win, otherwise only
/// when `stdout_is_tty`.
fn resolve_color(force: bool, no_color: bool, stdout_is_tty: bool) -> bool {
    if force {
        true
    } else if no_color {
        false
    } else {
        stdout_is_tty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_resolution() {
        assert!(resolve_color(true, false, false));
        assert!(resolve_color(true, true, false));
        assert!(!resolve_color(false, true, true));
        assert!(!resolve_color(false, false, false));
        assert!(resolve_color(false, false, true));
    }

    #[test]
    fn path_args_carry_value_hints() {
        use clap::CommandFactory;
        let command = Cli::command();
        let scan = command.find_subcommand("scan").unwrap();
        let paths = scan
            .get_arguments()
            .find(|arg| arg.get_id() == "paths")
            .unwrap();
        assert_eq!(paths.get_value_hint(), clap::ValueHint::AnyPath);

        let port = command.find_subcommand("port").unwrap();
        let editor = port
            .get_arguments()
            .find(|arg| arg.get_id() == "editor")
            .unwrap();
        assert_eq!(editor.get_value_hint(), clap::ValueHint::CommandName);
    }
}

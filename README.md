# todone

`todone` is a CLI for triaging marker comments (`TODO`, `FIXME`, ...) in a
repository. It scans your code with tree-sitter, walks you through every
finding in an interactive review, lets you **port** them into forge issues
(GitHub via the `gh` CLI in v1), and removes the ported comments from the
source — only after the issue actually exists on the forge.

```
$ todone scan
crates/todone-cli/src/cli.rs:7: TODO
    4 │
    5 │ use clap::{Args, Parser, Subcommand};
    6 │
 >  7 │ /// Triage TODO comments: scan, review, and port them to your forge.
    8 │ #[derive(Debug, Parser)]
    9 │ #[command(name = "todone", version, about, propagate_version = true)]
```

## Installation

`todone` is a Rust workspace; build it with cargo:

```sh
cargo build --release
# binary at target/release/todone
```

Command dependencies for v1:

| dependency | used for |
|---|---|
| `git` | repository root and `HEAD` discovery |
| `gh` | creating GitHub issues (`todone port`) |
| `$EDITOR` (default `nvim`) | drafting issues and read-only views |

## Usage

```
todone [--json] [--color|--no-color] <COMMAND>
```

### `todone scan`

Scan the repository (or a subpath) and print every marker comment with
context, like `grep -A/-B`:

```
todone scan                      # whole repo
todone scan src/ tests/          # only these paths
todone scan -A 1 -B 2 --pattern "TODO,FIXME,PERF" --no-space
todone scan --json               # machine-readable report
```

### `todone port`

The interactive flow. One screen per finding:

```
 1/3 — crates/todone-cli/src/cli.rs:7: TODO  (2 undecided)
┌──────────────────────────────────────────────────────────────┐
│ >  7 │ /// Triage TODO comments: scan, review, and port...    │
└──────────────────────────────────────────────────────────────┘
p port · s skip · d delete · o view · e select · j/k nav · c confirm · ? help · q quit
```

| key | action |
|---|---|
| `p` | **port** — opens the prefilled issue draft in `$EDITOR`; saving records the decision, quitting without changes returns to this finding |
| `s` / `d` | skip / delete (delete removes the comment without creating an issue) |
| `x` | clear the decision for this finding |
| `o` | open the file read-only in `$EDITOR` at the comment line (vim-family editors get `-R +line`) |
| `e` | edit the selected comment range: `←` shrink, `→` grow, `r` reset, `esc` done — for detaching attached comments |
| `j`/`k` | next / previous finding |
| `c` | confirmation screen: `y` execute, `b` back to review |
| `q` | quit (nothing is created or removed) |

The draft template contains the context snippet, the repository-relative
path, and the `HEAD` commit:

````markdown
---
tool = "todone"
version = 1
category = "TODO"
path = "crates/todone-cli/src/cli.rs"
commit = "3141b14"
---
# Title: <write title here>

## Context
```rust
/// Triage TODO comments: scan, review, and port them to your forge.
```

## Description
<write the issue description here>
````

Keep the `---` frontmatter, the `# Title:` line, and the `## Description`
section intact; everything else is free-form.

**Execution discipline.** No issue is created and no file is touched before
you confirm. When you do, each ported finding runs *create the issue first,
remove the comment only after it exists*; deletes just remove the comment.
Files are verified against a SHA-256 snapshot taken at scan time, so edits
made while reviewing are never clobbered (the issue is still created; the
removal is skipped and reported).

Headless variants:

```
todone port --auto skip|delete|port   # decide everything without the TUI
todone port --yes                     # skip the confirmation screen
todone port --dry-run                 # print the plan, change nothing
todone port --auto port --yes --json  # fully scriptable
```

### `todone config`

```
todone config        # effective configuration as TOML
todone config --sample
```

## Configuration

Layered, lowest to highest precedence:

1. built-in defaults
2. `$TODONE_CONFIG`, else `$XDG_CONFIG_HOME/todone/config.toml`, else
   `~/.config/todone/config.toml`
3. `<repo root>/todone.toml` (committed, shared with the team)
4. command-line flags

```toml
[scan.match]
categories = ["TODO", "FIXME"]   # priority order
case_sensitive = false           # "todo" matches "TODO"
require_space_before = true      # "// TODO" yes, "//TODO" no
require_colon = false            # require the ":" after the category

# paths = ["src"]                # default scope when none given on the CLI
# exclude = ["vendor/**"]        # glob patterns to skip

[context]
before = 3                       # context lines before a finding
after = 3                        # context lines after a finding

[forge]
kind = "github"                  # v1 only; the Forge trait is the seam

[editor]
# command = "nvim"               # falls back to $EDITOR
```

## `--json` output

`scan --json` emits a report with the repository, every finding (path,
category, line, selection byte range, the comment run, and the context
window), and scan statistics:

```json
{
  "repo": { "root": "/abs/path", "commit": "3141b14..." },
  "findings": [
    {
      "path": "src/lib.rs",
      "language": "rust",
      "category": "TODO",
      "line": 5,
      "selection": { "start": 55, "end": 66 },
      "comments": [ { "line": 5, "end_line": 5, "range": { "start": 55, "end": 66 }, "text": "// TODO: x" } ],
      "primary": 0,
      "context": [ { "line": 2, "text": "code", "kind": "before" } ]
    }
  ],
  "stats": { "files": 12, "findings": 3, "skipped_non_utf8": 0, "skipped_too_large": 0, "skipped_unsupported": 1 }
}
```

`port --json` emits the execution report: one entry per finding with the
action (`port`/`delete`/`skip`), the created issue (`number`, `url`),
whether the comment was removed, and any error.

## Architecture

```
crates/
├── todone-core/     domain model, tree-sitter scanning, matching, removal,
│                    layered config, issue-draft codec, session state machine
├── todone-forge/    Forge trait + GitHub backend (gh CLI) over a testable
│                    process layer (ScriptedRunner for tests)
└── todone-cli/      clap surface, scan/port/config runners, ratatui TUI,
                     highlighting (vendored tree-sitter queries + built-in
                     theme), editor sessions, JSON reports
```

- The session state machine in `todone-core` knows nothing about editors,
  terminals, or forges — the TUI, tests, and (later) a GUI drive it the
  same way.
- Removal operates on the set of tree-sitter comment-node byte ranges, so
  runs ending in a trailing comment (`let x = 1; // TODO`) keep their code.
- The `Forge` trait and `ProcessRunner` are the seams for dropping the `gh`
  dependency (direct GitHub HTTP) and adding GitLab/Gitea backends.

## Development

```sh
devenv shell          # gh, cargo-llvm-cov, neovim, git, rust toolchain
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo llvm-cov --workspace   # coverage (currently ~90% lines)
```

Pre-commit hooks (rustfmt, clippy, cargo test) run via devenv.

## Roadmap

- Drop the `gh` dependency: GitHub backend over the REST API directly.
- More forges: GitLab, Gitea, generic HTTP endpoints.
- Plain `--comment-chars` scanning as a fallback for languages without a
  grammar; user-supplied highlight themes.
- Issue labels and severity mapped from categories; deduplication of
  identical comments.
- GUI mode: a libghostty window with an editor pane and the todone pane,
  reusing the same session state.

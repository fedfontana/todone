# todone

`todone` is a CLI for triaging marker comments (`TODO`, `FIXME`, ...) in a
repository: it scans for them with tree-sitter, walks you through each one in
an interactive review, lets you port them into forge issues, and removes the
ported comments from the source.

The workspace contains:

- `crates/todone-core` — scanning, matching, removal, configuration, session state
- `crates/todone-forge` — issue-tracking forge abstraction and backends
- `crates/todone-cli` — the `todone` binary

See `docs/` for the design notes and roadmap.

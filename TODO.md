- if an empty (visual) line (even in the same syntactic same comment) is found, comment should be split. If next there's another comment, good, else stop
    ```rs
    // TODO: something
    // and some continuation
    //
    // FIXME: some other
    ```
    - also in the scenario above, probably neither of the comments should appear in the context (no matched comments in the context)

- there should be a way to set issue tags
- binds should be configurable
- status line should be configurable
- should have some custom comment selector where the user says "ill remove the comment content from the file" using $EDITOR
    - in some way this could help when/if no grammar can be loaded for the language for whatever reason
- verify read only view is really read-only
- more color on status line
- if nerd font (setting), then use some cooler chars
- precommit checks signal no clippy/linter files to run on, seems strange

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
- in the final page there should a live updating process while creating issues (e.g. show some details for each one as it is created, `[#2](github.com/owner/repo/issues/2) The title`)` as links, do they even work?)
- Should probably save some progress file somewhere and give the user a --continue flag, use may have command to save to specific path, and continue can take a path to contimnue from specific path (else auto discover in some state directory in XDG_DATA_DIR?)
- failed issue creations should not simply disappear, they should be reported in the final page and also be persisted somewhere so the user can edit them and retry (even in the same session)

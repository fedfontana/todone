- The following is marked as a single command, each line should be its own
    ```rs
    > 54 | // TODO: highlight the part of word that matches the query                                                                 │
    > 55 │ // FIXME: closing and quickly reopening the menu still shows the old query values for an instant before resetting to the em│
    > 56 │ // FIXME: when the input changes and the results change, I would love if the currently selected item stayed selected in the│
    > 57 │ // TODO: pages should also be findable here                                                                                │
    > 58 │ // TODO: should be able to toggle some settings straight from the command palette?                                         │
    > 59 │ // TODO: add help page for shortcuts?                                                                                      │
    > 60 │ // TODO: searching in the palette also proposes a feed of only items that match the given query?                           │
    > 61 │ // FIXME: should also include authors in the search
    ```

- if an empty (visual) line (even in the same syntactic same comment) is found, comment should be split. If next there's another comment, good, else stop
    ```rs
    // TODO: something
    // and some continuation
    //
    // FIXME: some other
    ```
    - also in the scenario above, probably neither of the comments should appear in the context (no matched comments in the context)

- currently `# Title: // TODO: highlight the part of word that matches the query`, not really correct, the matched comment part should not be part of the title
- there should be a way to set issue tags
- binds should be configurable
- status line should be configurable
- should have some custom comment selector where the user says "ill remove the comment content from the file" using $EDITOR
    - in some way this could help when/if no grammar can be loaded for the language for whatever reason
- verify read only view is really read-only
- content before the triggering comment should not be included in the selection of the comment


----



- The following is marked as a single command, each line should be its own
    ```rs
    > 54 | // TODO: highlight the part of word that matches the query                                                                 │
    > 55 │ // FIXME: closing and quickly reopening the menu still shows the old query values for an instant before resetting to the em│
    > 56 │ // FIXME: when the input changes and the results change, I would love if the currently selected item stayed selected in the│
    > 57 │ // TODO: pages should also be findable here                                                                                │
    > 58 │ // TODO: should be able to toggle some settings straight from the command palette?                                         │
    > 59 │ // TODO: add help page for shortcuts?                                                                                      │
    > 60 │ // TODO: searching in the palette also proposes a feed of only items that match the given query?                           │
    > 61 │ // FIXME: should also include authors in the search
    ```

- if an empty (visual) line (even in the same syntactic same comment) is found, comment should be split. If next there's another comment, good, else stop
    ```rs
    // TODO: something
    // and some continuation
    //
    // FIXME: some other
    ```

- currently `# Title: // TODO: highlight the part of word that matches the query`, not really correct, the matched comment part should not be part of the title
- content before the triggering comment should not be included in the selection of the comment

---
leftover issues:
- C-p does not work, p for port is triggered
- when text is reflowed for wrap, right now it overrides the next line rail, but it should not do that probably
- more color on status line
- if nerd font (setting), then use some cooler chars
- in some cases we still add the previous lines to the comments
    ```
    // Some real comment
    // TODO: some todo comment
    ```
    - we hightlight the first line as well
    - we should only highlitght the second
    - Example: 1/225 keepup, commons/src/provider.rs:273
- in some case we do not split subsequent todo comments:
    ```
    // TODO: this is one
    // TODO: this is the other
    ```
    - should be two
    - recognized as single comment
    - Example: keepup 6/225, commons/src/schemas/settings.ts:4
        - CommandPalette.tsx:54 with many comments one after the other works, even when two comments with the same type follow each other, so that is not the issue
- when we remove the TODO|FIXME comments from the context, we should expand the context until it spans the configured After/Before
- text content of the comment should not be truncated or have ellipsis added when in the title
- wrong pattern currently probably, the pattern for whitespace is `\s`, currently using `\w`

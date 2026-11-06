# notes-cli

A small note-taking CLI: argv parsing (`std/cli`), a JSON file (`std/fs` + `JSON`), search, tags
and optional ANSI colors.

```sh
velt build                       # -> target/velt/notes-cli
alias notes=./target/velt/notes-cli
notes add "Buy milk" -b "oat, 2 liters" -t home -t shop
notes list [--tag home] [--color]
notes search milk               # case-insensitive, title and body
notes show 1
notes tag 1 urgent
notes rm 1
notes tags                      # tag counts
```

The notes file is `--db <path>`, else `$NOTES_DB`, else `./notes.json`. Usage errors and unknown
ids print `notes: <message>` and exit 2.

| File | What |
|---|---|
| `src/store.velt` | `Note`, `NoteStore` (load/save, add, tag, remove, search, tag counts) |
| `src/commands.velt` | argv → one command → output lines (testable without a process) |
| `src/main.velt` | process glue: args, `$NOTES_DB`, clock, exit code |
| `tests/*.test.velt` | `velt test` |
| `demo.velt` / `demo.out` | scripted session; a golden in `cargo test --workspace --test golden` |

Tests and the demo import the sources as `"@notes/commands"` through the `paths` alias
`"@notes/*": "src/*"` in `package.velt`.

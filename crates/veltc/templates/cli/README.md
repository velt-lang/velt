# {{name}}

A command-line tool: argument parsing with `std/cli`, subcommands, `--help`, and tests that
call the command layer directly (no process needed).

```sh
velt run -- --help
velt run -- greet Ada --shout -n 2
velt run -- sum 1 2 3.5
velt test
velt build --release     # target/velt/{{name}}
```

Usage errors print `{{name}}: <message>` and exit with code 2.

| File | What |
|---|---|
| `src/commands.vlt` | the parser and the commands: argv → output lines (throws `CliError`) |
| `src/main.vlt` | process glue: `args()`, printing, exit code |
| `tests/commands.test.vlt` | `velt test` |

To add a command: add a `case` to `execute` in `src/commands.vlt`, describe it in `parser()`, and
test it in `tests/commands.test.vlt`.

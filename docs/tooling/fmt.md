# Formatter

```
velt fmt [<file|dir>...] [--check]
```

`velt fmt` formats Velt sources in place, with one style and no options, like `gofmt`. Without
paths it formats the package's `package.vlt` and `src/`, or every `.vlt` file under the current directory outside
a package; `target/` and hidden directories are skipped.

- `--check` writes nothing, lists the files that are not formatted, and exits with 1 if there
  are any. Use it in CI.
- A file that does not parse is reported and left alone (exit code 1).
- The style is close to Prettier's: 2-space indentation, a 100-column line width, double quotes
  (single quotes are kept when the text contains `"`), semicolons, trailing commas in lists that
  break over several lines, at most one blank line between statements. Template literals are
  printed verbatim. Formatting is idempotent and never changes the program.
- The language server formats documents on request ([Editors](editors.md)).

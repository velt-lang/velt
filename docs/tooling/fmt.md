# Formatter

```
velt fmt [<file|dir>...] [--check]
```

`velt fmt` formats Velt sources in place, with one style and no options, like `gofmt`. Without
paths it formats the package's `src/`, or every `.vlt` file under the current directory outside
a package; `target/` and hidden directories are skipped.

- `--check` writes nothing, lists the files that are not formatted, and exits with 1 if there
  are any. Use it in CI.
- A file that does not parse is reported and left alone (exit code 1).
- The style is close to Prettier's: 2-space indentation, a 100-column line width, double quotes
  (single quotes are kept when the text contains `"`), semicolons, trailing commas in lists that
  break over several lines, at most one blank line between statements. Template literals are
  printed verbatim. Formatting is idempotent and never changes the program.
- JSX is laid out like Prettier does it: an element that does not fit goes over several lines
  (in parentheses after `=`, `return` and `=>`), text is reflowed as a paragraph, and in an
  HTML element or a fragment a space next to a tag at a line break is written `{" "}` (it would
  otherwise be lost). A conditional with an element in it puts each element branch in
  parentheses when it breaks. Unlike Prettier, a component's children are kept as written (it
  receives them as its `children` prop, so `Save ` and `Save{" "}` differ: spaces next to its
  tags stay on their line), runs of several spaces in text are kept, branches other than
  elements are not given parentheses they did not have, and `<fbt>` gets no special layout.
- The language server formats documents on request ([Editors](editors.md)).

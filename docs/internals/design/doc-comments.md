# Design: doc comments

Status: implemented in `velt doc` (issue #513, part of #512) and in the language server (#514).
User documentation: [Lexical structure](../../reference/lexical.md#comments-and-semicolons),
[`velt doc`](../../tooling/cli.md#velt-doc).

## Problem

Before this change, any block of `//` or `///` lines right above a declaration was its
documentation. That has three faults:

- TypeScript developers write JSDoc (`/** … */`), which `velt doc` ignored. TypeScript's editor
  support, and the TypeScript standard library's declarations, use JSDoc too.
- Every ordinary comment above an export became documentation, including notes for the reader
  of the source (`// TODO`, "why this is fast"). There was no way to write a comment that is
  not documentation.
- The extractor scanned lines, not tokens: a `//` line inside a template string right above a
  declaration was read as its documentation.

There were also no tags, so the editor could not show the documentation of one parameter in
signature help (#514).

## Syntax

A **doc comment** is one of:

- a JSDoc block, `/** … */` (not `/**/` or `/*** … */`);
- a block of `///` lines on consecutive lines (not `////`).

It documents the declaration that follows it when it ends on the line right above the
declaration, or on the declaration's own line before it. Only whitespace, and plain comments on
the lines in between, may come between (see below). `export`, `async` and member modifiers
(`static`, `get`, `readonly`, …) belong to the declaration. A comment that starts after code on
its line is a trailing comment of that code, not a doc comment.

```ts ignore
/**
 * Splits `text` at each `separator`.
 *
 * @param text - the text to split
 * @param separator - what to split at; `""` splits into characters
 * @returns the parts, `[text]` when `separator` does not occur
 * @throws `RangeError` when `separator` is longer than 1 KiB
 * @example
 * split("a,b", ","); // ["a", "b"]
 * @see {@link join}
 */
export function split(text: string, separator: string): string[] { … }

/// The largest value `split` accepts as a separator, in bytes.
export const MAX_SEPARATOR: i64 = 1024;
```

Plain `//` and `/* */` comments are never documentation. Plain comments on the lines between a
doc comment and its declaration are skipped, as TypeScript does: lint and compiler directives
sit there (`// eslint-disable-next-line`, `// @ts-expect-error`, `// prettier-ignore`), and the
doc comment still documents the declaration.

```ts ignore
/** The default port. */
// eslint-disable-next-line no-magic-numbers
export const PORT: i64 = 8080;
```

A blank line between a comment and the declaration, or between a doc comment and the plain
comments after it, ends the association: a section comment followed by a blank line documents
nothing. **This differs from TypeScript**, which attaches a JSDoc comment across blank lines (and
also reads `/*** … */` as JSDoc). Velt keeps the stricter rule because the module doc depends on
it: the comment block at the top of a file documents the module exactly when a blank line
follows it, so with TypeScript's rule a file's leading comment would document the first
declaration as well. A blank line is also how a source file says that a comment is a section
heading or a note, not documentation. Ported TypeScript whose JSDoc is separated from its
declaration by a blank line loses that doc in `velt doc` until the blank line is removed.

Comments are found by the lexer (`velt_syntax::comment_ranges`), so comment markers inside
strings, templates, regular expressions and JSX text are never comments.

### Text

The text is Markdown (the subset of [`markdown.rs`](../../../crates/velt_doc/src/markdown.rs)).
In a `/** */` block the leading ` * ` of each line is removed (a line without `*` loses the
indentation the lines share); in a `///` block, `///` and one space. Indentation beyond that is
kept, so code blocks and nested lists work.

### Tags

A tag starts a line (after the comment marker) with `@name`, outside a code fence. It runs to
the next tag. The tags are JSDoc's, and TypeScript's editor reads the same ones:

| Tag | Meaning | Shown as |
|---|---|---|
| `@param name - text` | a parameter (`@param name text` and `@param {T} name text` too: the type is ignored, the signature has it; `[name]` and `[name=default]` give the name) | a **Parameters** list |
| `@returns text` (`@return`) | the result | **Returns:** text |
| `@throws text` (`@throws {E} text`) | an error it throws; one tag per error | **Throws:** `E`: text |
| `@example` | example code, verbatim to the next tag | a ` ```ts ` code block, unless the example has its own fence |
| `@deprecated [text]` | not to be used in new code | **Deprecated:** text, and the item is marked deprecated |
| `@see text` | a related item or URL | **See also:** text |

Inline `{@link name}` (also `{@linkcode}`, `{@linkplain}`) is rendered as `` `name` ``,
`{@link name text}` and `{@link name | text}` as the text, and a URL target as a Markdown link.
`{@link}` inside inline code or a code block is left alone. Links to items are not resolved yet;
`velt doc` could link them as it links type names in signatures.

Any other tag (`@since`, `@remarks`, `@typeParam`, …) is kept in the text as written. It is not
an error: the doc comment of TypeScript code ported to Velt keeps its information.

### Module docs

Unchanged: the comment block at the very top of a file (after a `#!` line, if any), followed by a
blank line or the end of the file, documents the module. It may be in any style (`//`, `///`,
`/* */`, `/** */`), since there is no declaration it could be mistaken for. Tags in it are read
too. Without the blank line, the block documents the first declaration instead.

## What the tools show

- **`velt doc`** (and `velt doc --std`, the docs website) shows the rendered Markdown under each
  item: the text, then the sections of the table above, in that order. A deprecated item's name
  is struck through and labelled "deprecated".
- **The editor** (`velt_lsp::docs`, [editors](../../tooling/editors.md#features)): hover shows
  the same Markdown under the signature; completion items show it when resolved (items of
  modules outside the program, inside `import { … }` and from auto-import, carry it in the
  list); signature help shows the description, returns and throws, and each parameter's
  `@param` text; deprecated items get the LSP `Deprecated` tag (struck through in completion
  lists), and uses of them a `Deprecated` hint. Each file's comment ranges and declaration
  index are built once per server process (keyed by the file's text), so a hover parses only the
  one comment it shows.

Both use one parser, `velt_doc::comment`: `doc_before(src, decl_lo)` finds and parses the doc
comment of the declaration starting at byte `decl_lo`; `doc_before_in` takes the comment ranges
when a caller looks up many declarations of one file; `parse` reads the text of one comment and
`module_doc` the module's. `DocComment::render_markdown` produces the Markdown both show.

## Migration

Plain `//` comments above declarations stop being documentation. std's were converted
mechanically: every `//` block that `velt doc` used as the documentation of an exported
declaration, an `extend` block or a public member became a `/** … */` block (one line when it
fits in 100 columns). Comments at the top of files, section comments separated by a blank line,
comments on private members and comments inside function bodies were left alone. The std API
reference built from the converted sources is byte for byte the same as before (the doc texts
were then corrected where they no longer matched the code). A test
(`crates/velt_doc/tests/std_docs.rs`) keeps it that way: it fails when a declaration that
`velt doc` documents has no doc comment while a plain comment ends right above it. `///` blocks
(`std/package.vlt`, the `websocket` template) keep working unchanged; the `lib` template uses
`/** */` with tags.

A package whose documentation is written in `//` loses it in `velt doc` until it is converted.
The language server will offer a quick fix, "Convert to doc comment", for a plain `//` block
right above an export (#518). There is no diagnostic: a `//` comment above an export is
legitimate.

## Not proposed

- Type information from tags (`@param {string}`, `@type`, `@template`): Velt's types are in the
  signature, and a second, unchecked source of types is a source of mismatches.
- Checking that `@param` names match the parameters. Possible later as a `velt check` lint.
- Doc tests (running `@example` code). Possible later; examples are shown as `ts` code, so they
  can be checked like the docs' `ts` blocks.

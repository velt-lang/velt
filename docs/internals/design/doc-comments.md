# Doc comments

Status: partly implemented. Issue: [#513](https://github.com/velt-lang/velt/issues/513).
User documentation:
[Lexical structure](https://veltlang.com/docs/reference/lexical/#comments-and-semicolons),
[`velt doc`](https://veltlang.com/docs/tooling/cli/#velt-doc).

## Summary

Documentation is written in JSDoc blocks (`/** … */`) or `///` lines directly above a
declaration, with JSDoc's tags (`@param`, `@returns`, `@throws`, …). `velt doc` and the language
server read them with one parser. Plain `//` and `/* */` comments are never documentation.

## Motivation

TypeScript developers write JSDoc, TypeScript's editor support reads it, and the TypeScript
standard library's declarations use it. Treating every `//` block above a declaration as its
documentation would leave no way to write a note for the reader of the source (`// TODO`, why a
loop is fast) that does not end up in the API reference. Tags give the editor the documentation of
one parameter for signature help.

## Design

### Syntax

A **doc comment** is one of:

- a JSDoc block, `/** … */` (not `/**/` or `/*** … */`);
- a block of `///` lines on consecutive lines (not `////`).

It documents the declaration that follows it when it ends on the line right above the
declaration, or on the declaration's own line before it. Only whitespace and plain comments on the
lines in between may separate them (see below). `export`, `async` and member modifiers (`static`,
`get`, `readonly`, …) belong to the declaration. A comment that starts after code on its line is a
trailing comment of that code, not a doc comment.

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

Plain comments on the lines between a doc comment and its declaration are skipped, as TypeScript
skips them: lint and compiler directives sit there (`// eslint-disable-next-line`,
`// @ts-expect-error`, `// prettier-ignore`), and the doc comment still documents the
declaration.

```ts ignore
/** The default port. */
// eslint-disable-next-line no-magic-numbers
export const PORT: i64 = 8080;
```

A blank line between a comment and the declaration, or between a doc comment and the plain
comments after it, ends the association: a section comment followed by a blank line documents
nothing. The module doc depends on this rule: the comment block at the top of a file documents
the module exactly when a blank line follows it. A blank line is also how a source file says that
a comment is a section heading or a note, not documentation.

Comments are found by the lexer (`velt_syntax::comment_ranges`), so comment markers inside
strings, templates, regular expressions and JSX text are never comments.

### Text

The text is Markdown (the subset of [`markdown.rs`](../../../crates/velt_doc/src/markdown.rs)).
In a `/** */` block the leading ` * ` of each line is removed (a line without `*` loses the
indentation the lines share); in a `///` block, `///` and one space. Indentation beyond that is
kept, so code blocks and nested lists work.

### Tags

A tag starts a line (after the comment marker) with `@name`, outside a code fence, and runs to the
next tag. The tags are JSDoc's, the ones TypeScript's editor reads:

| Tag | Meaning | Shown as |
|---|---|---|
| `@param name - text` | a parameter (`@param name text` and `@param {T} name text` too: the type is ignored, the signature has it; `[name]` and `[name=default]` give the name) | a **Parameters** list |
| `@returns text` (`@return`) | the result | **Returns:** text |
| `@throws text` (`@throws {E} text`) | an error it throws; one tag per error | **Throws:** `E`: text |
| `@example` | example code, verbatim to the next tag | a ` ```ts ` code block, unless the example has its own fence |
| `@deprecated [text]` | not to be used in new code | **Deprecated:** text, and the item is marked deprecated |
| `@see text` | a related item or URL | **See also:** text |

Inline `{@link name}` (also `{@linkcode}` and `{@linkplain}`) renders as `` `name` ``,
`{@link name text}` and `{@link name | text}` as the text, and a URL target as a Markdown link.
`{@link}` inside inline code or a code block is left alone.

Any other tag (`@since`, `@remarks`, `@typeParam`, …) is kept in the text as written. It is not an
error, so the doc comments of TypeScript code ported to Velt keep their information.

### Module docs

The comment block at the very top of a file (after a `#!` line, if any), followed by a blank line
or the end of the file, documents the module. It may be in any style (`//`, `///`, `/* */`,
`/** */`), since there is no declaration it could be mistaken for. Tags in it are read too.
Without the blank line, a `/** */` or `///` block documents the first declaration instead, and
plain comments document nothing.

### What the tools show

- **`velt doc`** (and `velt doc --std`) shows the rendered Markdown under each item: the text,
  then the sections of the table above, in that order. A deprecated item's name is struck through
  and labelled "deprecated". `velt doc --format json`, which the docs website renders, keeps the
  tags apart and resolves each `{@link}` to a name to the documented item it refers to (`null`
  when nothing documented has that name).
- **The editor** ([editors](https://veltlang.com/docs/tooling/editors/#features)): hover shows the
  same Markdown under the signature; completion items show it when resolved (items of modules
  outside the program, inside `import { … }` and from auto-import carry it in the list); signature
  help shows the description, returns and throws, and each parameter's `@param` text. Deprecated
  items get the LSP `Deprecated` tag (struck through in completion lists), and uses of them a
  `Deprecated` hint.

## Compilation

Both tools use one parser, `velt_doc::comment` (`crates/velt_doc/src/comment.rs`):
`doc_before(src, decl_lo)` finds and parses the doc comment of the declaration starting at byte
`decl_lo`; `doc_before_in` takes the comment ranges when a caller looks up many declarations of
one file; `parse` reads the text of one comment and `module_doc` the module's.
`DocComment::render_markdown` produces the Markdown both show.

The language server (`crates/velt_lsp/src/docs.rs`) indexes each file's declarations once and
keeps the index and the file's comment ranges per file, and across analyses by the file's text
(standard library files do not change between edits), so a hover parses only the comment it
shows.

std keeps its documentation in doc comments: `crates/velt_doc/tests/std_docs.rs` fails when a
declaration that `velt doc` documents has no doc comment while a plain comment ends right above
it.

## Compatibility

TypeScript attaches a JSDoc comment across blank lines and also reads `/*** … */` as JSDoc. Velt
does not, because with TypeScript's rule a file's leading comment would document both the module
and the first declaration. Ported TypeScript whose JSDoc is separated from its declaration by a
blank line loses that doc in `velt doc` until the blank line is removed.

A package whose documentation is written in `//` comments has none in `velt doc` until it is
converted. There is no diagnostic: a `//` comment above an export is legitimate.

## Alternatives considered

**Type information from tags** (`@param {string}`, `@type`, `@template`). Velt's types are in the
signature, and a second, unchecked source of types is a source of mismatches.

## Limitations

- `@param` names are not checked against the parameters.
- `@example` code is not run as a test.
- The HTML pages of `velt doc` render a `{@link}` to a name as code; only the JSON output resolves
  it to the item.

## Not yet implemented

- **Quick fix.** The language server offers "Convert to doc comment" for a plain `//` block right
  above an export ([#518](https://github.com/velt-lang/velt/issues/518)).

# Design: a package manifest written in Velt

Status: implemented (issue #128), except `std/package.vlt` with its sync test (waiting for
`Record`, #17) and the language server's package-graph refresh. The maintainer's review answered
the open questions; the answers are in [Decisions](#decisions). The contract is
[manifest.md](../contracts/manifest.md), which replaced `velt_toml.md`.

## Problem

The package manifest is `velt.toml`, Cargo-style. TypeScript developers know JSON manifests
(`package.json`, `tsconfig.json`), not TOML, and neither format gets help from the language
server: a misspelled key or an invalid `[paths]` pattern only fails when `velt` runs, as a plain
string error without a location. Velt already has a parser, a type checker and a language
server, so it could check the manifest the same way it checks code.

The manifest must still be read the way JSON is read: `vpm`, the registry server, the language
server and third-party tools load it without compiling or running anything, and a package
install never runs package code.

## Proposal

**File.** `package.vlt` at the package root replaces `velt.toml`. Its fields are flat and
camelCase, like `package.json`:

```ts ignore
import type { Package } from "velt:package";

// Comments are allowed anywhere.
export const pkg: Package = {
  name: "todo-api",
  version: "0.3.0",
  entry: "src/main.vlt",                      // optional, the default
  registry: "https://registry.example.com",   // optional; only the root package's is used
  dependencies: {
    sqlite: "^1.2",                           // registry package, semver requirement
    http: { version: "0.3" },
    util: { path: "../util" },                // local package
  },
  paths: { "@app/*": "src/*", "@config": "src/config" },
  jsx: { importSource: "velt:jsx" },
  native: { targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] },
};
```

Every field keeps the meaning and validation of the `velt.toml` field it replaces
([manifest.md](../contracts/manifest.md)). `[package]`'s `name`, `version` and `entry` move to
the top level, and the `[dependencies]`, `[paths]`, `[jsx]` and `[native]` tables become
`dependencies`, `paths`, `jsx` and `native`. Package names stay `[a-z][a-z0-9_-]*`; scoped names
such as `@velt/sqlite` are a separate issue.

**Types.** `velt:package` (`std/package.vlt`) contains only types:

```ts ignore
export interface Package {
  name: string;
  version: string;
  entry?: string;
  registry?: string;
  dependencies?: Record<string, Dependency>;
  paths?: Record<string, string>;
  jsx?: Jsx;
  native?: Native;
}

export type Dependency = string | DependencySource;

export interface DependencySource {
  version?: string;
  path?: string;
}

export interface Jsx {
  importSource?: string;
}

export interface Native {
  path?: string;          // default "native"
  targets?: string[];     // triples from vpm::manifest::NATIVE_TARGETS
  wasm?: boolean;         // must be false for now
}
```

- **Doc comments.** Each field has one, so hovering in an editor shows that field's rules.
- **`native`.** It mirrors the `[native]` table that native packages (#98) add.
- **Future fields.** `devDependencies` and `workspace` are added when those features exist.
  Adding an optional field doesn't break existing manifests.
- **Prerequisite: `Record<K, V>` (#17).** The language doesn't have it yet. An object literal
  can't stand in for a `Map` (`Map` is built with `new Map()`), so `Map<string, …>` is no
  alternative. Only `std/package.vlt` and the sync test below need `Record`; the reader doesn't,
  and can land first.

**The data-only subset.** The reader checks the syntax tree and accepts only this:

- **Imports.** At most one `import type { … } from "velt:package"`. Any other import, and any
  value import, is an error.
- **The declaration.** Exactly one `export const pkg: Package = <value>;`. The binding is always
  named `pkg`, so docs and tools can say "`pkg` in `package.vlt`". The `: Package` annotation is
  required, so editors always check the file. No other statements are allowed: no functions, no
  other declarations, no `let`.
- **Values.** `<value>` and everything inside it is one of:
  - a string literal (`"…"` or `'…'`);
  - a number literal;
  - `true` or `false`;
  - an array literal;
  - an object literal whose keys are identifiers or string literals.
- **Rejected.** Everything else is an error at its own location: template literals, `null`,
  spreads, computed and shorthand keys, methods, identifiers, calls, `new`, operators, `as`, and
  a key written twice.
- **Allowed extras.** Comments and trailing commas.

Reading a manifest therefore never resolves a name, evaluates an expression or touches the
environment.

**Reading.** The reader lives in `vpm::manifest` and is the only definition of a valid
manifest:

1. `velt_syntax::parse_file` parses the file. It needs no standard library root and no
   compiler, so the registry server and `vpm` can call it.
2. The subset check turns the syntax tree into a small value tree (string, number, bool, array,
   object), each value with its span.
3. A decoder maps the value tree onto the existing `Manifest` structs. Unknown keys are errors
   everywhere, not only in `[jsx]` and `[native]` as today.
4. The field checks (name, version, `entry`, registry, dependencies, `paths::check_alias`, the
   `native` checks) run on the result and carry spans.

Errors are `velt_common::Diagnostics` with a file and a span, printed like compiler errors
(`package.vlt:3:12: error: …`), instead of the old string messages without a location.

The reader rejects everything the `Package` type rejects, so the command line, `vpm` and the
registry never need the type checker. A test keeps the reader and the type in sync: a manifest
that uses every field must type-check against `std/package.vlt` and must pass the reader.

**Untrusted input.** The registry server reads manifests from uploaded archives, so the reader
assumes hostile input:

- **Size.** A manifest larger than 64 KiB is refused before it is parsed.
- **Nesting.** The parser's existing depth limit (`MAX_DEPTH`) applies. `parse_file` already
  runs the parser on its own thread, with a stack sized for that depth.
- **Count.** A manifest with more than 10,000 values (array elements, object properties and
  scalars) is refused.

Each limit is an error with a location, never a crash.

**Editors.** The language server already analyzes every open `.vlt` file. Type-checking against
`Package` gives completion, hover, go-to-definition and errors without any manifest-specific
code.

**Editing (`velt add`).** Today `velt add` edits `velt.toml` with `toml_edit`, which keeps
comments. The new version works like this:

1. Splice text at syntax-tree spans: replace the value of an existing dependency key, or insert
   `name: "req"` before the closing `}` of `dependencies`, adding a `,` after the last property
   when it has none (and a `dependencies` property when there is none).
2. Format the file with `velt_fmt`, which keeps comments.
3. Read the result again to validate it.

As today, the original text is restored if the install fails.

**JSON for other tools.** `velt manifest --json` prints the validated manifest as JSON (the
`Package` shape), for tools that can't link `velt_syntax`.

**What stays.**

- These stay TOML: `velt.lock`, the registry's `index.toml`, and the `native.toml` inside a
  native bundle. They are generated, nobody edits them, and they are protocol, not manifests.
- Build or dev scripts, if they are ever added, are separate explicit files and never run on
  install.

## Comparison

| | `velt.toml` (today) | `velt.json` + JSON schema | `package.vlt` |
|---|---|---|---|
| TypeScript familiarity | low: TOML tables | high: like `package.json` | high: a TypeScript object literal |
| Comments | yes | no (or JSONC, which many tools reject) | yes |
| Typing and editor support | none today; needs a TOML extension and a schema | completion and validation through a published schema and the editor's JSON support | completion, hover, go-to-definition and errors from the Velt language server, with no extra setup |
| Errors from `velt` | strings without a location | JSON parser and schema errors with a location | Velt diagnostics with a location and snippet |
| Safety | data only | data only | data only, enforced by the subset check; nothing runs |
| Third-party tools | TOML parsers everywhere | JSON parsers everywhere | `velt manifest --json`, or link `velt_syntax` |
| Parse speed | fast | fast | fast: one lexer and parser pass over a small file, no type checking |
| Comment-preserving edits | `toml_edit` | none (JSON has no comments to lose) | splice plus `velt_fmt` |
| Migration cost | none | the same as `package.vlt` | every manifest, template, example, golden test and document that shows one, the CLI help, the Rust tests that write manifest text, and `vpm`'s TOML manifest code |

The real cost of `package.vlt` is tools written in other languages: every language reads TOML
and JSON, but only Velt's parser reads a `.vlt` file. `velt manifest --json` closes that gap.
Every other row is as good as or better than the alternatives, and the issue asks for this:
the manifest written in the same language as the code.

## Compiler and tool changes

- `std/package.vlt`: the `Package` types, plus a row in `std/README.md` (once #17 has landed).
- `vpm`:
  - `MANIFEST_FILE` becomes `package.vlt`.
  - The reader replaces `toml::from_str` and `Manifest::to_toml`.
  - `find_package_root` looks for `package.vlt`.
  - `scaffold::manifest_text` writes `package.vlt` text.
  - `edit.rs` splices instead of using `toml_edit`, which is dropped. `toml` stays for the lock
    file, the index and `native.toml`.
  - The archive whitelist (`archive.rs`) and the content hash (`contents.rs`) cover
    `package.vlt`.
- `velt_registry`: unchanged apart from going through the new reader. It rejects an archive
  that contains a `velt.toml`.
- `veltc`:
  - `velt init`, `velt new` and the templates follow `scaffold::manifest_text`.
  - The help text changes, as does the "`[jsx] importSource` in velt.toml" label.
  - New: `velt manifest --json`.
  - `velt dev` and `velt test` watch `package.vlt`.
  - `velt fmt` also formats `package.vlt`.
- Language server:
  - Rebuild the cached package graph when `package.vlt` is saved; today it is built once per
    session.
  - Leave `package.vlt` out of workspace symbols.

## Diagnostics

Each error points at the text that caused it:

- `the manifest is data only: calls are not allowed`. Identifiers, operators, template literals,
  spreads and computed keys get the same message with their own name.
- `` `null` is not allowed; leave the key out ``. TypeScript users will write `registry: null`.
- `` `package.vlt` may only import types from `velt:package` ``.
- `` the manifest must be one `export const pkg: Package = { … }` ``.
- `` unknown key `dependecies` in the manifest ``, with a "did you mean" when a key is close.
- `` duplicate key `sqlite` ``.
- `the manifest is larger than 64 KiB`, and `the manifest has more than 10000 values`.
- The existing checks, now with a location: an invalid name, a version that isn't semver, a bad
  `paths` pattern, an unknown native target, and so on.

## Migration

There is no backward compatibility. If a directory has a `velt.toml` and no `package.vlt`, every
command that looks for a package stops with:

``error: `velt.toml` is no longer read; the manifest is `package.vlt` ``

The fix-it prints the equivalent `package.vlt`, converted from the old file. There is no
`velt migrate` command: the converter is temporary, and a command would outlive it.

Everything in the repository moves in the same change:

- the templates and the code behind `velt init`/`velt new`;
- the example apps and the golden tests that carry a manifest;
- the documents that show a manifest, and `docs/site/pages.txt`;
- the CLI help;
- the Rust tests that write manifest text.

The contract `contracts/velt_toml.md` becomes `contracts/manifest.md`. `docs/tooling/manifest.md`
is rewritten, with the `jsx` and `native` sections the current page lacks.

Implementation order:

1. **The reader in `vpm`**, with unit tests. These include hostile-input tests: deep nesting, a
   file over the size limit, and too many keys (#135).
2. **The switch, `velt add` and the migration**, in one pull request: once `vpm` reads only
   `package.vlt`, every `velt.toml` in the repository and `velt add`'s TOML editing break, so
   steps 2 to 4 of the original plan cannot land separately. It moves `vpm`, `veltc`, the
   registry and the file watchers over, adds the migration error and `velt manifest --json`,
   makes `velt add` splice, and migrates the templates, examples, golden tests and docs.
3. **Follow-ups:** `std/package.vlt` and the sync test once #17 has landed (until then an editor
   reports `velt:package` as unknown in a `package.vlt`), and the language server's
   package-graph refresh when `package.vlt` is saved.

## Not proposed

- **Executable manifests**, with computed values, environment access or imports of other code.
  The manifest stays as safe as JSON.
- **`export default`**. Velt has named exports only.
- **`satisfies Package`** instead of an annotation. Velt has no `satisfies`, and the annotation
  does the same job here.
- **Keeping `velt.toml` as an alternative.** Two formats would double the tool code and the
  docs.
- **A `velt migrate` command.** The fix-it prints the new file instead.

## Decisions

From the maintainer review on issue #128:

1. **The binding name is fixed:** `export const pkg`. There is one way to write it.
2. **The migration fix-it only prints the new file.** There is no `velt migrate` command.
3. **`velt manifest --json` exists**, added in step 2. It answers the third-party tools row of
   the comparison.
4. **`velt.lock`, `index.toml` and `native.toml` stay TOML.**
5. **Scoped package names (`@scope/name`) are a separate issue.**
6. **`Record<K, V>` (#17) is a prerequisite** for `std/package.vlt` and the sync test, not for
   the reader.
7. **The registry's limits:** at most 64 KiB, the parser's `MAX_DEPTH`, and at most 10,000
   values.

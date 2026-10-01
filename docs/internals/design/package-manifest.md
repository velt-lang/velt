# Design: a package manifest written in Velt

Status: proposed (issue #128). Nothing here is implemented. It replaces the contract
[velt_toml.md](../contracts/velt_toml.md) and needs maintainer review before any code changes.

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
};
```

Every field keeps today's meaning and validation ([velt_toml.md](../contracts/velt_toml.md)):
`[package]`'s `name`, `version` and `entry` move to the top level, and the `[dependencies]`,
`[paths]` and `[jsx]` tables become `dependencies`, `paths` and `jsx`. Package names stay
`[a-z][a-z0-9_-]*`; scoped names such as `@velt/sqlite` are a separate question.

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
}

export type Dependency = string | DependencySource;

export interface DependencySource {
  version?: string;
  path?: string;
}

export interface Jsx {
  importSource?: string;
}
```

Each field has a doc comment, so hover in an editor shows the rules for that field. The type
lists only the fields that exist. `devDependencies`, `native` (#22) and `workspace` are added
when those features land; adding an optional field doesn't break existing manifests. The type
needs `Record<K, V>` (#17), optional fields and unions, all of which the language has.

**The data-only subset.** A manifest may contain only the following, checked on the syntax tree:

- At most one `import type { … } from "velt:package"`. Any other import, and any value import,
  is an error.
- Exactly one `export const <name>: Package = <value>;`. The `: Package` annotation is required,
  so editors always check the file. There are no other statements: no functions, no other
  declarations, no `let`.
- `<value>` and everything inside it is one of:
  - a string literal (`"…"` or `'…'`),
  - a number literal,
  - `true` or `false`,
  - an array literal,
  - an object literal whose keys are identifiers or string literals.
- Everything else is an error at its own location: template literals, `null`, spreads, computed
  and shorthand keys, methods, identifiers, calls, `new`, operators, `as`, and a key written
  twice.
- Comments and trailing commas are allowed.

So reading a manifest never resolves a name, evaluates an expression or touches the
environment.

**Reading.** The reader lives in `vpm::manifest` and is the only definition of what a valid
manifest is:

1. `velt_syntax::parse_file` parses the file. It needs no standard library root and no
   compiler, so the registry server and `vpm` can call it.
2. The subset check turns the syntax tree into a small value tree (string, number, bool, array,
   object), each value with its span.
3. A decoder maps the value tree onto the existing `Manifest` structs. Unknown keys are errors
   everywhere; today only `[jsx]` rejects them.
4. The existing checks (`Manifest::validate`, `paths::validate`) run on the result and now
   carry spans.

Errors are `velt_common::Diagnostics` with a file, a span and a source snippet, printed like
compiler errors, instead of today's string messages without a location. The reader rejects
everything the `Package` type rejects, so the command line, `vpm` and the registry never need the
type checker. A test keeps the two in sync: a manifest that uses every field must type-check
against `std/package.vlt` and must also pass the reader.

**Editors.** The language server already analyzes every open `.vlt` file. Type-checking against
`Package` gives completion, hover, go-to-definition and errors without any manifest-specific
code.

**Editing (`velt add`).** Today `velt add` edits `velt.toml` with `toml_edit`, which keeps
comments. Instead it splices text at syntax-tree spans: it replaces the value of an existing
dependency key, or inserts `name: "req",` before the closing `}` of `dependencies` (adding a
`dependencies` property when there is none). It then formats the file with `velt_fmt`, which
keeps comments, and reads the result again to validate it. As today, the original text is
restored if the install fails.

**What stays.**
- `velt.lock` stays a generated TOML file.
- The registry's `index.toml` stays TOML; it is protocol, not a manifest.
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
| Third-party tools | TOML parsers everywhere | JSON parsers everywhere | need `velt_syntax`, or JSON output from `velt` (open question 3) |
| Parse speed | fast | fast | fast: one lexer and parser pass over a small file, no type checking |
| Comment-preserving edits | `toml_edit` | none (JSON has no comments to lose) | splice plus `velt_fmt` |
| Migration cost | none | the same as `package.vlt` | 5 manifests, 18 documents, templates, help text, about 24 test files and `vpm`'s TOML code |

The real cost of `package.vlt` is the third-party row. Every language reads TOML and JSON, but
only Velt's own parser reads a `.vlt` file. Tools that need the manifest and can't link
`velt_syntax` would ask `velt` for JSON. Every other row is as good as or better than the
alternatives, and the "same language everywhere" argument is the point of the issue.

## Compiler and tool changes

- `std/package.vlt`: the `Package` types, plus a row in `std/README.md`.
- `vpm`:
  - `MANIFEST_FILE` becomes `package.vlt`.
  - The reader replaces `toml::from_str` and `Manifest::to_toml`.
  - `find_package_root` looks for `package.vlt`.
  - `edit.rs` splices instead of using `toml_edit`, which is dropped (`toml` stays for the lock
    file and the index).
  - The archive whitelist (`archive.rs`) and the content hash (`contents.rs`) cover
    `package.vlt`.
- `velt_registry`: unchanged apart from going through the new reader; it rejects an archive
  that contains a `velt.toml`.
- `veltc`:
  - `scaffold::manifest_text` writes `package.vlt`, so `velt init`, `velt new` and the
    templates follow.
  - Help text and the "`[jsx] importSource` in velt.toml" label change.
  - `velt dev` and `velt test` watch `package.vlt`.
  - `velt fmt` also formats `package.vlt`.
- Language server: rebuild the cached package graph when `package.vlt` is saved (today it is
  built once per session), and leave `package.vlt` out of workspace symbols.

## Diagnostics

Each error points at the offending text:

- `the manifest is data only: calls are not allowed` (likewise for identifiers, operators,
  template literals, spreads and computed keys);
- `` `package.vlt` may only import types from `velt:package` ``;
- `` the manifest must be one `export const <name>: Package = { … }` ``;
- `` unknown key `dependecies` in the manifest `` (with a "did you mean" when one is close);
- `` duplicate key `sqlite` ``;
- the existing checks, now with a location: an invalid name, a version that isn't semver, a bad
  `paths` pattern, and so on.

## Migration

There is no backward compatibility. If a directory has a `velt.toml` and no `package.vlt`, every
command that looks for a package stops with:

``error: `velt.toml` is no longer read; the manifest is `package.vlt` ``

with a fix-it that prints the equivalent `package.vlt`, converted from the old file. The
converter is a short function that lives only as long as the migration needs it.

Everything in the repository moves in the same change:

- the 4 example apps and `tests/golden/lang/jsx_package`;
- the 18 documents that mention `velt.toml`, and `docs/site/pages.txt`;
- the CLI help text;
- the Rust tests that write manifest text.

The contract `contracts/velt_toml.md` becomes `contracts/manifest.md`. `docs/tooling/manifest.md`
is rewritten, and gains the `jsx` section it lacks today.

Implementation order, one pull request each:

1. `std/package.vlt` and the reader, with unit tests.
2. Switch `vpm`, `veltc`, the registry and the file watchers over, and add the migration error.
3. `velt add` by splicing.
4. Migrate the templates, examples, golden tests and docs, and refresh the language server's
   package graph.

## Not proposed

- **Executable manifests** (computed values, environment access, imports of other code). The
  manifest stays as safe as JSON.
- **`export default`**: Velt has named exports only.
- **`satisfies Package`** instead of an annotation: Velt has no `satisfies`, and the annotation
  does the same job here.
- **Keeping `velt.toml` as an alternative**: two formats double the tool code and the docs.

## Open questions

1. Should the binding name be fixed (`export const pkg`), so that tools and docs are uniform, or
   may it be any name?
2. Should the migration fix-it only print the new file, or should a `velt migrate` command write
   it and delete `velt.toml`?
3. Should `velt manifest --json` print the manifest as JSON for tools that can't link
   `velt_syntax`?
4. Should `velt.lock` and the registry's `index.toml` stay TOML? (Proposed: yes; they are
   generated and nobody edits them.)
5. Should scoped package names (`@scope/name`) be their own issue?

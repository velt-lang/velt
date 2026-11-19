# A package manifest written in Velt

Status: implemented. Issue: [#128](https://github.com/velt-lang/velt/issues/128).

## Summary

A package's manifest is `package.velt` at the package root: one exported object literal, typed
`Package`, with flat camelCase fields like `package.json`. It is written in the language the
package is written in, gets comments, and gets editor diagnostics, completion and hover from the
Velt language server, including live data from the package registry.

The manifest is still read the way JSON is read. `velt`, the registry server, the language server
and other tools read it without compiling or running anything: a reader accepts only a data-only
subset of Velt syntax, so installing a package never runs package code. The contract, with every
field, is [manifest.md](../contracts/manifest.md).

## Motivation

TypeScript developers know JSON manifests (`package.json`, `tsconfig.json`). A TOML manifest
(Cargo-style) is unfamiliar to them and gets no help from the language server: a misspelled key
or an invalid `paths` pattern only fails when a command runs. Velt already has a parser and a
language server, so the manifest can be checked with the same tools and the same diagnostics as
code, at the place where the mistake is.

## Design

### The file

```ts ignore
import type { Package } from "velt/package";

// Comments are allowed anywhere.
export const pkg: Package = {
  name: "todo-api",
  version: "0.3.0",
  entry: "src/main.velt",                      // optional, the default
  registry: "https://registry.example.com",   // optional; only the root package's is used
  dependencies: {
    sqlite: "^1.2",                           // registry package, semver requirement
    http: { version: "0.3" },
    util: { path: "../util" },                // local package
  },
  paths: { "@app/*": "src/*", "@config": "src/config" },
  jsx: { importSource: "velt/jsx" },
};
```

The binding is always `export const pkg`, so docs and tools can say "`pkg` in `package.velt`"
and there is one way to write a manifest. The `: Package` annotation is required.

### The types

`velt/package` (`std/package/manifest.velt` and `std/package/workspace.velt`, re-exported by
`std/package/index.velt`) contains only types: `Package` and the types of its fields
(`Dependency`, `DependencySource`, `Jsx`, `Native`, `CLibrary`, `Import`, `Host`, `AllowNative`,
`Lint`, `Build`), plus `Workspace` for a workspace manifest ([workspaces.md](workspaces.md)).
Every field has a doc comment. The types document the manifest (`velt doc`) and make the import
resolve; they are not what checks a manifest, in the editor or anywhere else. Adding an optional
field does not break existing manifests.

### The data-only subset

The reader checks the syntax tree and accepts only this:

- **Imports.** At most one `import type { … } from "velt/package"`. Any other import, and any
  value import, is an error.
- **The declaration.** Exactly one `export const pkg: Package = <value>;` and no other
  statements: no functions, no other declarations, no `let`.
- **Values.** `<value>` and everything inside it is a string literal (`"…"` or `'…'`), a number
  literal, `true` or `false`, an array literal, or an object literal whose keys are identifiers
  or string literals.
- **Rejected.** Everything else is an error at its own location: template literals, `null`,
  names, calls, `new`, operators, `as`, parentheses, regular expressions, spreads, computed and
  shorthand keys, methods, and a key written twice.
- **Allowed extras.** Comments and trailing commas.

Reading a manifest therefore never resolves a name, evaluates an expression or touches the
environment.

### Untrusted input

The registry server reads manifests from uploaded archives, so the reader assumes hostile input:

- **Size.** A manifest larger than 64 KiB is refused before it is parsed.
- **Nesting.** The parser's depth limit applies; `velt_syntax::parse_file` runs the parser on a
  thread with a stack sized for that depth.
- **Count.** A manifest with more than 10,000 values (array elements, object properties and
  scalars) is refused.

Each limit is an error with a location, never a crash.

### Editors

Type-checking `package.velt` as a program would not give useful editor support:

- **False errors.** Sema allows only constant expressions in a module-level `const`, because a
  module constant has no storage and its initializer is evaluated at every use. Records and
  arrays allocate, so any manifest with `dependencies` or `paths` would get "module-level
  constants must be constant expressions". The rule is right for programs and meaningless for a
  file that is never compiled.
- **No key completion.** The language server completes members after `x.` and names in scope,
  not object keys from an expected type.
- **No live data.** Version completion and "a newer version exists" need the registry, which no
  type can provide.

So the language server treats `package.velt` as a document kind of its own
(`crates/velt_lsp/src/manifest.rs`):

1. **Routing.** A document named `package.velt` is not analyzed as a program: no loading and no
   sema. `velt` never compiles it either. It is left out of workspace symbols.
2. **Diagnostics** come from `vpm::manifest::read`, the function every `velt` command uses: the
   editor shows exactly what `velt build` would, at the same places.
3. **Position.** The reader's value tree carries spans, so the cursor maps to a path: a key at
   the top level, the value of `dependencies.sqlite`, an element of `native.targets`.
4. **Schema.** `vpm::manifest::schema` is one table of the manifest's fields (key, kind, doc
   text, allowed values). Completion offers the keys valid at the cursor that are not written
   yet and fixed values; hover shows a field's doc. A test checks that `std/package/` declares
   the same fields.
5. **Live registry data**, from the package's registry, fetched in the background by
   `crates/velt_lsp/src/registry.rs` and cached per session:
   - completion of versions inside a dependency's requirement, newest first, skipping yanked
     versions;
   - completion of package names at a new key in `dependencies`, through the registry's search
     (`vpm::search`), writing the whole entry (`sqlite: "^0.3.1"`);
   - hover on a dependency: the latest version, whether the requirement matches it, the locked
     version, whether the package has native code;
   - diagnostics when no published version matches or the package is not in the registry, and a
     hint with a quick fix when a newer version is out of range.

   An offline or unreachable registry makes these features quiet, never errors. Requirements are
   checked against the versions that are not yanked plus the one `velt.lock.json` pins, as
   resolution does. The pure parts (where the cursor is, what the data means) are
   `vpm::manifest::ide` and `vpm::manifest::ide::registry`.

Formatting goes through `velt fmt`, and go-to-definition of `Package` works because the type
import resolves.

### Editing

`velt add` edits the manifest without losing comments (`crates/vpm/src/edit.rs`):

1. It splices text at syntax-tree spans: it replaces the value of an existing dependency key, or
   inserts `name: "req"` before the closing `}` of `dependencies`, adding a `,` after the last
   property when it has none (and a `dependencies` property when there is none).
2. It formats the file with `velt_fmt`, which keeps comments.
3. It reads the result again to validate it.

The original text is restored if the install fails.

### JSON for other tools

`velt manifest --json` prints the validated manifest as JSON in the `Package` shape, with
defaults filled in, for tools that cannot link `velt_syntax`.

Generated files are not manifests: `velt.lock.json`, the registry's `index.json` and a native
bundle's `native.json` are protocol that nobody edits, so they are JSON.

## Compilation

`vpm::manifest` (`crates/vpm/src/manifest/`) is the only definition of a valid manifest:

1. `velt_syntax::parse_file` parses the file. It needs no standard library and no compiler, so
   the registry server and `vpm` can call it.
2. The subset check (`read.rs`) turns the syntax tree into a small value tree (string, number,
   bool, array, object), each value with its span.
3. A decoder maps the value tree onto the `Manifest` structs (`mod.rs`). Unknown keys are errors
   everywhere.
4. The field checks (name, version, `entry`, registry, dependencies, `paths`, `imports`,
   `native`, `allowNative`, …) run on the result and carry spans.

Errors are `velt_common::Diagnostic`s with a file and a span, printed like compiler errors
(`package.velt:3:12: error: …`). The workspace manifest, `velt.workspace.velt`, goes through the
same reader with its own binding and type (`read/workspace.rs`).

## Diagnostics

Each error points at the text that caused it:

- `the manifest is data only: calls are not allowed`; names, operators, template literals,
  `new`, `as`, parentheses, regular expressions, spreads and methods get the same message with
  their own wording, and a shorthand property suggests `` write `name: …` ``.
- `` `null` is not allowed; leave the key out ``.
- `` `package.velt` may only import types from `velt/package` `` and
  `` `package.velt` may have at most one import ``.
- `` the manifest must be one `export const pkg: Package = { … }` ``.
- `` unknown key `dependecies` in the manifest ``, with a `did you mean` note when a key is
  close.
- `` duplicate key `sqlite` `` and `` the manifest is missing `version` ``.
- `the manifest is larger than 64 KiB` and `the manifest has more than 10000 values`.
- The field checks: an invalid name, a version that is not semver, a bad `paths` pattern, an
  unknown native target, and so on.

### Migration from `velt.toml`

`velt.toml` is not read. A directory that has one and no `package.velt` is still found as a
package root, and every command that reads it stops with
`` `velt.toml` is no longer read; the manifest is `package.velt` `` and a help note that prints
the converted `package.velt` (`vpm::manifest::legacy`). There is no `velt migrate` command. A
published package or archive that contains a `velt.toml` cannot be installed or served; the
error says its author must publish a new version.

## Compatibility

`package.velt` is valid TypeScript: a type import and an annotated object literal. A
`package.json` is never a Velt package's manifest (it is read only for npm packages compiled from
their TypeScript sources); the field names follow `package.json` where a field means the same
thing (`name`, `version`, `dependencies`, `imports`).

## Alternatives considered

**`velt.toml`.** TOML is data only and has comments, but TypeScript developers do not know its
tables, and its editor support needs a separate extension and schema. Errors from a TOML parser
know nothing about Velt's field rules.

**`velt.json` with a JSON Schema.** Editors' JSON support gives static completion and validation
from a published schema, and every language has a JSON parser. But live registry data still
needs custom language-server code, JSON has no comments (JSONC is rejected by many tools), and
the manifest would not be written in the package's own language. `velt manifest --json` covers
tools in other languages.

**Executable manifests**, with computed values, environment access or imports of other code. A
manifest the registry and installer read must be as safe as JSON.

**`export default`** for the manifest. The manifest is found by its binding name; one named
binding is the one way to write it.

**`satisfies Package`** instead of an annotation. Velt has no `satisfies`, and the annotation
does the same job here.

**Exempting `package.velt` from sema's constant rule** so a type check passes. It would hide the
false error but still give no key completion and no live data.

**`definePackage({ … })`**, as in Vite's `defineConfig`. A call is not data, and it adds no
completion over the annotation.

## Limitations

- Only Velt's parser reads a `.velt` file; tools in other languages use
  `velt manifest --json`.
- Build or dev scripts are not part of the manifest, and nothing in a manifest runs on install.

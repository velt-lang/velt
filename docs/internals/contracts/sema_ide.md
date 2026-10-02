# Sema IDE query API (`velt_sema::ide`)

Producer: semantics agent (`crates/velt_sema/src/ide/`). Consumer: `crates/velt_lsp` (and any other
editor tooling). Public items below are the contract; everything else in the module is internal.

## Entry point
```rust
pub fn check_for_ide(modules: &[SourceModule], root: usize) -> ide::Analysis
```
- Same inputs as `velt_sema::check` (the driver's loader output; the AST may come from a parser that
  recovered from syntax errors).
- Runs every checking pass of `check` (collect, bodies, ownership, throws, JSON, moves) but:
  errors never stop other items from being checked, `modules[root]` does not need `main`, and no
  HIR is built. Never panics on user input (same guarantee as `check`).
- While checking it records side tables (name → definition, expression → type, local scopes); the
  returned `Analysis` owns everything, so it does not borrow `modules`.
- Cost: one extra branch per hook when compiling (`check` records nothing); `check_for_ide` stores a
  few words per name use and per checked expression.

## `Analysis`
All offsets are byte offsets into the file's source; a span "covers" an offset when
`lo <= offset <= hi` (a cursor right after an identifier still names it). Where several recorded
spans cover the offset, the shortest wins.

| Method | Result |
|---|---|
| `diagnostics() -> &[Diagnostic]` | All diagnostics of the program (every module), warnings included; never "`main` not found". |
| `def_at(file, offset) -> Option<DefRef>` | The definition named at the offset: a use (local, item, field, method, getter, static field, variant, type name, import name, struct-literal / pattern field, JSX tag or attribute name, in a closing tag too) or a declaration itself. |
| `type_at(file, offset) -> Option<String>` | Type of the innermost checked expression (or declaring identifier of a local) at the offset, spelled as source: `Map<string, i64>`, `Point`, `{ x: i64 }`, `T` (generic params by name), `(x: i64) => string` (function values show parameter names when known, else `arg0`). Never `adt#N`. |
| `scope_at(file, offset) -> Vec<(String, DefRef)>` | Names visible at the offset: locals (innermost first; a local is visible from its declaration to the end of its block), block-level (nested) items, the module's items and imports, then prelude exports. Shadowed names appear once. |
| `members_of_type_at(file, offset) -> Vec<(String, DefRef, String)>` | Members usable on the expression at the offset: `(name, definition, type)`. A value offers its type's fields, getters and methods (inherited, interface defaults, `extend` blocks such as the prelude's `Array` methods), `T \| null` / `shared<T>` those of `T`. A type name (`Math`, `Color`) offers its static methods, static fields and enum variants. Private members are not offered. The third element is the member's type (fields, getters) or signature `(a: A) => R`, with the receiver's type arguments substituted. |
| `namespace_members(file, ns) -> Vec<(String, DefRef)>` | The exports of namespace import `ns` of the file (`import * as ns from "…"`), re-exports included, sorted by name; empty if `ns` is not one. `def_at` on `x` in `ns.x` (expressions and types) is the export's definition. |
| `members_of(def: &DefRef) -> Vec<(String, DefRef, String)>` | Same listing for a definition: instance members of a local's / parameter's / constant's / field's type (fields of generic types excepted), static members of a type. Object types (`{ x: i64 }`) list their fields. |
| `jsx_intrinsics(file) -> &[(String, DefRef, String)]` | The intrinsic tags of the file's JSX runtime (the fields of `JSX.IntrinsicElements`, docs/internals/contracts/jsx.md): `(tag, field definition, attribute type)`, sorted by tag. Empty when the file has no JSX runtime (it contains no JSX, or the runtime is invalid). `members_of` on a tag's definition lists its attributes; `def_at` on a tag or attribute name in an element is the same field. |
| `throws_of(def: &DefRef) -> Option<&str>` | What a function, method, constructor or closure-valued variable throws (declared or inferred), spelled like `detail` (`NotFound \| Forbidden`); `None` when it throws nothing or `def` is not callable. |
| `mutation_of(def: &DefRef) -> Option<&Mutation>` | What a function, method or constructor modifies, as ownership inference decided: `Mutation { this: bool, params: Vec<String> }` (`this`: a method that modifies its receiver, never a constructor; `params`: parameters whose contents it modifies, in declaration order; `any()`: either). `None` for anything else, closures included (callbacks take their arguments by a fixed convention). |
| `references(def: &DefRef) -> Vec<Span>` | Every span naming `def` (its declaration included) in all modules, sorted, deduplicated. Uses through an import alias are included (their text is the alias). |

## `DefRef`
```rust
pub struct DefRef {
    pub name: String,
    pub kind: DefKind,
    pub span: Span,     // the declaring identifier (Span::DUMMY for compiler-provided defs)
    pub module: usize,  // index of the declaring module in `modules`
    pub detail: String, // one line for hover/completion, e.g. `function f(a: i64): string`
}
impl DefRef { pub fn same_def(&self, other: &DefRef) -> bool } // identity: span + kind + name
pub enum DefKind { Function, ExternFunction, Method, StaticMethod, Getter, Constructor, Field,
    StaticField, Constant, Struct, Class, Interface, Enum, Variant, TypeAlias, Local, Parameter }
impl DefKind { pub fn is_type(self) -> bool } // struct, class, interface, enum, type alias
```
`detail` formats: `function f<T>(a: T, b?: i64): R` (`?` = has a default; a function or method that can
throw ends with ` throws E`, its declared or inferred error type), `(method) User.greet(): string`,
`(static) Math.sqrt(x: f64): f64`, `(getter) Map.size: usize`, `constructor User(name: string)`,
`(field) Point.x: i64`, `(static) Math.PI: f64`, `const LIMIT: i64`, `class User`, `struct Point<T>`,
`interface Named`, `enum Color { Red, Green = 5 }`, `Color.Green = 5`, `type Id`, `let x: i64` / `const x: i64`,
`(parameter) p: string`. Two `DefRef`s for the same definition may differ in `detail` (inherited
members spelled with different type arguments): compare with `same_def`.

## Known limits (POC)
- Locals of a closure are scoped like any block; `this` is a local named `this` whose declaring
  span is the method name.
- Members of values whose type is a generic parameter (`x: T extends Shape`) are not listed yet.
- Builtin methods implemented by intrinsics (`push`, `pop`, `clone`, `length`) have no `DefRef`
  and are not listed.

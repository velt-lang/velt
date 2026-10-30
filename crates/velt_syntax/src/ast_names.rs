//! Names the parser gives to constructs it writes as ordinary syntax (re-exported by `ast`).

/// `import.meta`: the parser reads it as this name, and adds a module constant of that name (not
/// one a program can write), `new __ImportMeta("", "", false, "")` ([`IMPORT_META_CLASS`]),
/// whose arguments the driver, which knows the module's file and whether it is the program's
/// root, fills in: the directory, the path, whether it is the root, and the `file:` URL.
pub const IMPORT_META: &str = "import.meta";

/// The fields of `import.meta`, in Node's order: the module's directory, its path, whether it is
/// the program's entry module, the function that resolves a specifier against it, and its
/// `file:` URL (Node's `import.meta.dirname`, `filename`, `main`, `resolve` and `url`).
pub const IMPORT_META_FIELDS: [&str; 5] = ["dirname", "filename", "main", "resolve", "url"];

/// The local name of the class `import.meta` is an instance of: the parser imports
/// `ImportMeta` from [`IMPORT_META_MODULE`] under it into a module that mentions `import.meta`.
pub const IMPORT_META_CLASS: &str = "__ImportMeta";

/// The std module that exports the class of `import.meta`.
pub const IMPORT_META_MODULE: &str = "velt/lang/import_meta";

/// The prelude function an object-literal property with a computed key known only at run time
/// is written as a spread of: `{ [k]: v }` is `{ ...__computed_key(k, v) }` (a one-entry
/// `Record<string, V>`). The callee's span is the bracketed key, which tells it from a written
/// call.
pub const COMPUTED_KEY_FN: &str = "__computed_key";

/// The prelude function a getter in an object literal is written as a call of: `{ get v() {
/// body } }` is `{ v: __object_get(() => { body }) }`, the getter as an arrow (its span starts
/// at the name, as a method's does). The callee's span is the `get` keyword, which tells it
/// from a written call. Sema reads it where the literal implements an interface.
pub const OBJECT_GET_FN: &str = "__object_get";

/// [`OBJECT_GET_FN`] for a setter: `{ set v(x) { body } }` is `{ v: __object_set((x) => { body
/// }) }`, the callee's span being the `set` keyword.
pub const OBJECT_SET_FN: &str = "__object_set";

/// The parameter the parser puts in front of the parameters of an object-literal method whose
/// body uses `this` (outside the functions and classes declared in it): `{ m(x) { return
/// this.n + x; } }` is `{ m: (this, x) => { ... } }`. The same goes for getters, setters,
/// generator methods and `function` expressions that are a property's value. No written
/// parameter can have this name (it is a keyword), and the parameter's span is empty, at the
/// start of the method. Sema passes the object the method is called on as this parameter where
/// it knows the literal's type, and reports `this` elsewhere.
pub const METHOD_THIS: &str = "this";

/// Is `name` the [`METHOD_THIS`] parameter the parser added?
pub fn is_method_this(name: &crate::ast::Ident) -> bool {
    name.name == METHOD_THIS && name.span.lo == name.span.hi
}

/// Member name of the computed key `[Symbol.dispose]` (a method declaration
/// `[Symbol.dispose]() {}` or an access `x[Symbol.dispose]`): an `Ident` with this exact
/// name. No plain identifier can collide with it.
pub const SYMBOL_DISPOSE: &str = "[Symbol.dispose]";

/// Member name of the computed key `[Symbol.asyncDispose]` (see [`SYMBOL_DISPOSE`]).
pub const SYMBOL_ASYNC_DISPOSE: &str = "[Symbol.asyncDispose]";

/// Member name of the computed key `[Symbol.iterator]` (see [`SYMBOL_DISPOSE`]).
pub const SYMBOL_ITERATOR: &str = "[Symbol.iterator]";

/// Member name of the computed key `[Symbol.asyncIterator]` (see [`SYMBOL_DISPOSE`]).
pub const SYMBOL_ASYNC_ITERATOR: &str = "[Symbol.asyncIterator]";

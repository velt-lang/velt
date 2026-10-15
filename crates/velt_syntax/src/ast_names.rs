//! Names the parser gives to constructs it writes as ordinary syntax (re-exported by `ast`).

/// `import.meta`: the parser reads it as this name, and adds a module constant of that name (not
/// one a program can write) holding an object literal with the string fields
/// [`IMPORT_META_FIELDS`], all empty. The driver, which knows the module's file, fills them in.
pub const IMPORT_META: &str = "import.meta";

/// The fields of `import.meta`, in order: the module's `file:` URL, its path and its directory
/// (Node's `import.meta.url`, `filename` and `dirname`).
pub const IMPORT_META_FIELDS: [&str; 3] = ["url", "filename", "dirname"];

/// The prelude function an object-literal property with a computed key known only at run time
/// is written as a spread of: `{ [k]: v }` is `{ ...__computed_key(k, v) }` (a one-entry
/// `Record<string, V>`). The callee's span is the bracketed key, which tells it from a written
/// call.
pub const COMPUTED_KEY_FN: &str = "__computed_key";

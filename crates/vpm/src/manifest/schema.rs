//! The fields of `package.vlt`, in one table: the reader's known keys, and what editors complete
//! and explain ([`super::ide`]). `std/package.vlt` declares the same fields as Velt types (a test
//! keeps them in line).

use super::NATIVE_TARGETS;

/// One field of an object in the manifest.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub key: &'static str,
    pub kind: Kind,
    /// Its type as `std/package.vlt` writes it.
    pub ty: &'static str,
    /// Whether the field must be written.
    pub required: bool,
    pub doc: &'static str,
}

/// What a field holds.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Str,
    Bool,
    /// An array of strings; when `values` is not empty, each is one of them.
    StrArray {
        values: &'static [&'static str],
    },
    /// An object with these fields.
    Object(&'static [Field]),
    /// An object whose keys the user picks (package names, alias patterns).
    Map(Entry),
}

/// What a [`Kind::Map`] maps its keys to.
#[derive(Clone, Copy, Debug)]
pub enum Entry {
    /// A string (`paths` targets).
    Str,
    /// A semver requirement, or an object with [`DEPENDENCY`]'s fields.
    Dependency,
}

/// The manifest object: `export const pkg: Package = { … }`.
pub const PACKAGE: &[Field] = &[
    Field {
        key: "name",
        kind: Kind::Str,
        ty: "string",
        required: true,
        doc: "The package name: lowercase letters, digits, `_` and `-`, starting with a letter.",
    },
    Field {
        key: "version",
        kind: Kind::Str,
        ty: "string",
        required: true,
        doc: "A semantic version, such as `\"0.1.0\"`.",
    },
    Field {
        key: "description",
        kind: Kind::Str,
        ty: "string",
        required: false,
        doc: "One line about the package, shown by `velt search` and registry listings: at most \
              300 characters, no line breaks, no surrounding whitespace.",
    },
    Field {
        key: "keywords",
        kind: Kind::StrArray { values: &[] },
        ty: "string[]",
        required: false,
        doc: "Search words, such as `[\"json\", \"parser\"]`: at most 10, each lowercase \
              letters, digits and `-` (at most 32 characters).",
    },
    Field {
        key: "entry",
        kind: Kind::Str,
        ty: "string",
        required: false,
        doc: "The program's root file, a `/`-separated path inside the package \
              (default `\"src/main.vlt\"`).",
    },
    Field {
        key: "registry",
        kind: Kind::Str,
        ty: "string",
        required: false,
        doc: "The registry for dependencies and `velt publish`: an `http://` or `https://` URL \
              (default: the local registry; `VELT_REGISTRY` takes precedence). Only the root \
              package's is used.",
    },
    Field {
        key: "dependencies",
        kind: Kind::Map(Entry::Dependency),
        ty: "Record<string, Dependency>",
        required: false,
        doc: "Dependencies by package name: a semver requirement (`\"^1.2\"`) or \
              `{ version?, path? }`.",
    },
    Field {
        key: "paths",
        kind: Kind::Map(Entry::Str),
        ty: "Record<string, string>",
        required: false,
        doc: "Import aliases, like TypeScript's `compilerOptions.paths`: \
              `{ \"@app/*\": \"src/*\" }`. A pattern and its target have at most one `*`, at \
              the end; targets stay inside the package.",
    },
    Field {
        key: "jsx",
        kind: Kind::Object(JSX),
        ty: "Jsx",
        required: false,
        doc: "How the package's modules compile JSX.",
    },
    Field {
        key: "native",
        kind: Kind::Object(NATIVE),
        ty: "Native",
        required: false,
        doc: "A Rust crate in the package, built into a native library.",
    },
];

/// The object form of a dependency.
pub const DEPENDENCY: &[Field] = &[
    Field {
        key: "version",
        kind: Kind::Str,
        ty: "string",
        required: false,
        doc: "A semver requirement.",
    },
    Field {
        key: "path",
        kind: Kind::Str,
        ty: "string",
        required: false,
        doc: "A local package directory, relative to this manifest.",
    },
];

/// `jsx`.
pub const JSX: &[Field] = &[Field {
    key: "importSource",
    kind: Kind::Str,
    ty: "string",
    required: false,
    doc: "The module whose `jsx-runtime` compiles JSX: a dependency, `\"velt:jsx\"` (the \
          default), a `paths` alias, or `./dir` relative to the package root. A \
          `// @jsxImportSource` comment in a file wins.",
}];

/// `native`.
pub const NATIVE: &[Field] = &[
    Field {
        key: "path",
        kind: Kind::Str,
        ty: "string",
        required: false,
        doc: "The crate's directory, one directory name in the package root (default \
              `\"native\"`).",
    },
    Field {
        key: "targets",
        kind: Kind::StrArray {
            values: NATIVE_TARGETS,
        },
        ty: "string[]",
        required: false,
        doc: "The targets `velt publish` publishes a prebuilt library for.",
    },
    Field {
        key: "wasm",
        kind: Kind::Bool,
        ty: "boolean",
        required: false,
        doc: "WebAssembly libraries are not supported yet: must be `false` (the default).",
    },
];

/// The field `key` of `fields`.
pub fn field(fields: &'static [Field], key: &str) -> Option<&'static Field> {
    fields.iter().find(|f| f.key == key)
}

/// The fields of the object at `path` (keys from the manifest object down; a dependency's name
/// stands for itself), or `None` when its keys are the user's (`dependencies`, `paths`) or it is
/// not an object.
pub fn object_at(path: &[String]) -> Option<&'static [Field]> {
    let mut fields = PACKAGE;
    let mut rest = path;
    while let Some((key, tail)) = rest.split_first() {
        match field(fields, key)?.kind {
            Kind::Object(inner) => fields = inner,
            Kind::Map(Entry::Dependency) => {
                // `dependencies.<name>` is a dependency object.
                let (_, tail) = tail.split_first()?;
                if !tail.is_empty() {
                    return None;
                }
                return Some(DEPENDENCY);
            }
            _ => return None,
        }
        rest = tail;
    }
    Some(fields)
}

/// The field at `path` (its last key), when the schema names it.
pub fn field_at(path: &[String]) -> Option<&'static Field> {
    let (key, parent) = path.split_last()?;
    field(object_at(parent)?, key)
}

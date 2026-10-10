//! Debug descriptions of source variables and their types (invariant 10 in vir.rs).
//! CONTRACT FILE — maintainer-owned (see docs/internals/contracts/README.md).
//!
//! Lowering fills them only when asked to (`LowerOptions::debug_info`); without them
//! `Program::debug_types` is empty and every `LocalDecl::debug` is `None`. Backends turn them into
//! debugger variables (DWARF `DW_TAG_variable` with a type); nothing else reads them.
//!
//! A debug type names a source type and says how its value is laid out, in terms of the
//! layouts the program already has: fields are VIR field indexes of an aggregate (its
//! `AggLayout` gives their offsets and VIR types). A scalar's own VIR type is its natural
//! representation; optimizations may store a variable in a narrower one (a `number` counter as
//! an integer), so for a local a backend takes the encoding from the local's VIR type.

use super::{AggId, SrcLoc, Ty};

/// Index into `Program::debug_types`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DebugTyId(pub u32);

/// The source variable a local holds.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalDebug {
    /// Where the variable is declared (its name, for params and bindings).
    pub decl: SrcLoc,
    pub ty: DebugTyId,
    /// The local holds a pointer to the value (a by-reference param or binding, a captured
    /// variable, a variable in a shared cell), not the value itself.
    pub by_ref: bool,
    /// The variable is a parameter of the function.
    pub param: bool,
}

/// A source type as a debugger shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugTy {
    /// The type as written in source: `number`, `string`, `Point`, `number[]`, `Point | null`.
    pub name: String,
    pub kind: DebugKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DebugKind {
    /// A number, integer or `boolean`, naturally stored as this scalar type (a local holding
    /// it may have a narrower one, which wins).
    Scalar(Ty),
    /// `string`: `Agg(STR_AGG)` (rt_abi.md "Strings").
    Str,
    /// `T[]`: the array aggregate `{ data: ptr, len: u64, cap: u64 }` of `T` elements.
    Array { elem: DebugTyId },
    /// A struct, anonymous object or tuple stored inline in `agg`.
    Struct { agg: AggId, fields: Vec<DebugField> },
    /// A class: a pointer to its heap object `obj` (fields after the vtable pointer, if any).
    Class { obj: AggId, fields: Vec<DebugField> },
    /// An enum without payloads: an `I64` whose value is a member's (numeric enums: the
    /// member's value; string enums: its index).
    Enum { members: Vec<(String, i64)> },
    /// An enum with payloads, a union or a `Result`: the base aggregate `agg` starts with the
    /// tag (the variant index), and variant `i` is laid out as its view aggregate.
    Tagged {
        agg: AggId,
        variants: Vec<DebugVariant>,
    },
    /// `T | null`: a pointer that is null for none (`repr` = `Ptr`), a presence flag (`Bool`,
    /// when `T` stores nothing), or `{ some: bool, value: T }` (`Agg`).
    Option { inner: DebugTyId, repr: Ty },
    /// `shared<T>`: a pointer to `{ count: u64, value: T }` (`boxed`).
    Shared { boxed: AggId, inner: DebugTyId },
    /// Shown as its raw words: function values and closures, interface values, promises, maps,
    /// boxed recursive values; stored as this VIR type.
    Opaque(Ty),
}

/// A named field: VIR field `index` of the enclosing aggregate.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugField {
    pub name: String,
    pub index: u32,
    pub ty: DebugTyId,
}

/// One variant of a tagged type: its name and fields in its view aggregate.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugVariant {
    pub name: String,
    pub view: AggId,
    pub fields: Vec<DebugField>,
}

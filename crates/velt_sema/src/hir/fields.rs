//! A type's fields: `FieldDef`, and `FieldInit`, an initializer a class gives a field it
//! inherits; `DynMember`, a method or accessor found by name through `any`.

use super::{DefId, Expr, TyId};

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TyId,
    /// Default initializer (class field `= expr`, or `null` for optional fields).
    pub default: Option<Expr>,
    /// Declared optional (`x?: T`): it may be absent. JSON and printing leave an absent
    /// (`null`) one out, as JavaScript leaves out a missing key, whereas a `T | null` field
    /// holding `null` is written; a spread copies it only while present.
    pub optional: bool,
    /// Declared `private` (in this type or the base class that declares it), or an ES private
    /// field (`#x`). Interface fields are never private.
    pub private: bool,
    /// An optional field whose declared type is nullable (`a?: T | null`) in a struct or object
    /// type: it keeps "absent" apart from a present `null` with a presence flag, which lowering
    /// stores after the fields. A literal leaves it absent with `Intrinsic::FieldAbsent`, a
    /// write makes it present, and `Intrinsic::FieldPresent` reads the flag.
    pub presence: bool,
    /// Not enumerable: a symbol brand `Object.defineProperty` adds (velt_sema
    /// `defined_props`). `console.log` and spreads leave it out, as Node does.
    pub hidden: bool,
}

/// An initializer a class gives a field it inherits: an error class's `name = "AppError"`
/// sets the prelude `Error`'s `#name` (`AdtDef::inherited_inits`).
#[derive(Clone, Debug)]
pub struct FieldInit {
    /// The field it stores (an index into `AdtDef::fields`).
    pub field: u32,
    /// Depth of the class declaring it: 0 for a class without a base class, 1 for its
    /// subclasses, and so on. It runs with that class's initializers.
    pub depth: u32,
    /// It runs right before the initializer of field `at`, one of the declaring class's own
    /// fields (that class's field count: after the last), after any earlier one with the
    /// same `at`.
    pub at: u32,
    pub init: Expr,
}

/// A method or accessor of a struct / class / object type, found by name when a member of an
/// `any` holding such a value is read, called or written (`Program::dyn_members`); or one of
/// its `readonly` fields, which a write through `any` must not change.
#[derive(Clone, Debug)]
pub struct DynMember {
    /// The member's name (`size`, `get`); never an ES private `#name`.
    pub name: String,
    pub kind: DynMemberKind,
    /// The method's `Def::Fn` (`this` first); the type's own `Def::Adt` for a
    /// `ReadonlyField`.
    pub def: DefId,
    /// The type declaring it, in terms of the type's own parameters (`Base<T>` for a method a
    /// class inherits from `Base<T>`): its type arguments are the method's leading ones.
    pub owner: TyId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynMemberKind {
    Method,
    Getter,
    Setter,
    /// A field declared `readonly` (TypeScript checks it statically; Velt relies on it, e.g.
    /// to narrow `o.x !== null` across calls, so a write through `any` is refused).
    ReadonlyField,
}

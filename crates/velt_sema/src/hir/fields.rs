//! A type's fields: `FieldDef`, and `FieldInit`, an initializer a class gives a field it
//! inherits.

use super::{Expr, TyId};

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

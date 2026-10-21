//! The operators of deferred types ([`super::TyKind::Op`]).

/// The operator of a [`super::TyKind::Op`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TyOp {
    /// A generic type alias (sema's alias #n) whose body (a conditional, mapped, indexed access
    /// or template literal type) needs its arguments, applied to the `Op`'s arguments.
    Alias(u32),
    /// `A & B & …` with a part that is a type parameter or another deferred type (`T & { $set:
    /// (v: T) => void }`, #350): the `Op`'s arguments are the parts in written order, and it
    /// reduces to the object type `crate::intersections` builds from them.
    Intersect,
    /// `{ ...t, $set }` with a part known only per instantiation (#350): the parts' types in
    /// order (explicit properties as object types), reduced to the spread's object type; its
    /// value is an `Intrinsic::SpreadObject` (hir_encodings.md "Spreads and conversions").
    Spread,
    /// Never a value's type: the requirement that an `Upcast` from the first argument to the
    /// second keeps one object type at every instantiation (reduced to the second, #350). One
    /// per conversion written (sema's conversion #n), so each is reported where it is.
    Convert(u32),
    /// A conditional, mapped, indexed access, `keyof` or utility type written in a generic
    /// definition, not in an alias, that needs its type parameters (`{ [K in keyof M]: V<M[K]> }`
    /// in a signature, #350): sema's form #n, its arguments the definition's type parameters.
    Inline(u32),
}

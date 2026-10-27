//! `Program::fresh_closures` (#943).

use super::TyId;

/// What lowering is told about a closure for the comparison of function values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshClosure {
    /// Each evaluation is a new function where JavaScript has one: a generic function value
    /// converted to another function type, a function of fixed types asserted to a generic one,
    /// and a generic function's value at a type over the caller's type parameters. `.0` is the
    /// function type of its values: lowering reports one made in a program that compares
    /// function values of that type (or of one it converts to).
    New(TyId),
    /// One of several values of one function: a generic function's constant at a generic
    /// function type (`.0`) when the function is also a value at another type, or instantiated
    /// at a function type of fixed types. Its values are one value at that type, but differ from
    /// the function's others: lowering reports one made in a program that compares `unknown` or
    /// `any` values while one holds such a value.
    Several(TyId),
    /// A function value of type `from` converted to function type `to` by this closure calling
    /// it: a value made by a [`FreshClosure::New`] closure may be compared as a `to`.
    Converts { from: TyId, to: TyId },
}

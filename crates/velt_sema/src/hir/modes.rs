//! How values are passed, used and converted: the `PassMode` of a parameter, `Conversion` of an
//! object literal and the `UseMode` sema's ownership analysis gives a place read.

/// Whether an `AdtLit` converts an object to another object type (hir_encodings.md "Object
/// conversions"): its fields are then each absent or a read of one source place, and a
/// source holding a typed proxy gives that proxy instead of the literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Conversion {
    /// An ordinary literal.
    No,
    /// A conversion: a plain source is copied.
    Copy,
    /// A conversion whose copy the program could tell from the original (a later write, `===`,
    /// ...), allowed only because the program creates proxies: a plain source throws a
    /// `TypeError` instead of being copied.
    ProxyOnly,
}

/// How a place is used by the expression that reads it (sema's ownership analysis).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UseMode {
    Copy,
    /// Ownership leaves the place; lowering must not drop it at scope end (drop flags if conditional).
    Move,
    Borrow,
    BorrowMut,
}

/// How an argument is passed / a parameter is received. Decided by sema, part of the fn ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassMode {
    /// Bitwise copy (Copy types: ints, floats, bool, Copy structs).
    Copy,
    /// Callee receives a read-only pointer; caller keeps ownership. Default for non-Copy params.
    Borrow,
    /// Callee receives a mutable pointer: sema inferred that it may modify the value (a param
    /// whose contents the body mutates, a method that mutates `this`; see hir_encodings.md).
    BorrowMut,
    /// Ownership moves into the callee (callee drops it).
    Owned,
}

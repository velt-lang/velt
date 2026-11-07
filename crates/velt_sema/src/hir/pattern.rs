//! Patterns (M2+ draft).

use velt_common::Span;

use super::{DefId, Lit, LocalId, TyId, UseMode};

#[derive(Clone, Debug)]
pub struct Pat {
    pub kind: PatKind,
    pub ty: TyId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wildcard,
    Binding(LocalId, UseMode),
    Lit(Lit),
    Variant {
        def: DefId,
        variant: u32,
        args: Vec<Pat>,
    },
    /// Field index → sub-pattern.
    Adt {
        fields: Vec<(u32, Pat)>,
    },
    Tuple(Vec<Pat>),
    Array {
        elems: Vec<Pat>,
        rest: Option<LocalId>,
    },
    Or(Vec<Pat>),
    /// `null` pattern on Option.
    None,
    /// Non-null pattern on Option.
    Some(Box<Pat>),
    /// `instanceof C`: the class object (or the interface value holding one) is an instance of
    /// class `C` or of one of its subclasses; it tests the dynamic class and binds nothing. Only
    /// on a class whose subclasses include `C`, or on an interface value.
    InstanceOf(DefId),
}

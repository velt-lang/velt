//! Items holding other items (part of the `ast` contract): `declare global` / `declare module`
//! blocks and namespaces.

use velt_common::Span;

use super::{Ident, Item};

#[derive(Clone, Debug)]
pub struct Namespace {
    pub name: Ident,
    /// Type aliases, interfaces and namespaces (exported or not; only exported ones are
    /// visible outside the namespace). The parser reports values (`const`, `function`, …).
    pub items: Vec<Item>,
    /// `declare namespace N { … }`
    pub declare: bool,
}

#[derive(Clone, Debug)]
pub struct Augment {
    /// `declare module "spec"`: the module specifier (resolved like an import's) and its span;
    /// `None` for `declare global`.
    pub module: Option<(String, Span)>,
    pub items: Vec<Item>,
}

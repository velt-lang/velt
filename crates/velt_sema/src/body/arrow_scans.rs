//! What scans of an arrow function's syntax find, kept per arrow (by span) for the function
//! being checked. An arrow tried against each member of a union of function types
//! (`expr/closure.rs` `member_by_trial`) is checked once per try, and so is every arrow nested
//! in it: scanning each one's syntax again at every check would cost its size per check, which
//! nesting multiplies (#859). Arrows are told apart by span and parse node: one the checker
//! makes up (`NodeId(u32::MAX)`) is scanned each time.

use std::rc::Rc;

use rustc_hash::FxHashMap;
use velt_common::Span;
use velt_syntax::ast;

use super::assign_ends::assignment_ends;
use super::assigned::assigned_by_closures;

/// An expression the parser made, by span and node.
type Key = (Span, ast::NodeId);

/// The key of `e`, unless the checker made it up (it may share a span with another).
fn key(e: &ast::Expr) -> Option<Key> {
    (e.id != ast::NodeId(u32::MAX)).then_some((e.span, e.id))
}

/// Looks up `e` in `map`, scanning it (and keeping the result) when it isn't there.
fn kept<T: Clone>(map: &mut FxHashMap<Key, T>, e: &ast::Expr, scan: impl FnOnce() -> T) -> T {
    match key(e) {
        Some(k) => map.entry(k).or_insert_with(scan).clone(),
        None => scan(),
    }
}

/// The kept results, per arrow.
#[derive(Default)]
pub(crate) struct ArrowScans {
    /// The names an arrow takes from its surroundings, and `this`, sorted.
    free: FxHashMap<Key, Rc<[String]>>,
    /// The variables closures created in an arrow assign (`Frame::closure_assigned`).
    assigned: FxHashMap<Key, FxHashMap<String, Span>>,
    /// Where each variable an arrow assigns is last assigned (`Frame::assign_ends`).
    ends: FxHashMap<Key, FxHashMap<String, u32>>,
    /// Whether a closure in the initializer of a `const` names the constant
    /// (`late_consts::closure_uses`), per initializer and name.
    late: FxHashMap<(Key, String), bool>,
}

impl ArrowScans {
    /// The names arrow `e` takes from its surroundings, and `this`, sorted.
    pub(crate) fn free_names(&mut self, e: &ast::Expr) -> Rc<[String]> {
        kept(&mut self.free, e, || {
            let mut names: Vec<String> = velt_syntax::scope::free_in_expr(e)
                .into_iter()
                .map(|id| id.name.clone())
                .chain(std::iter::once("this".to_string()))
                .collect();
            names.sort_unstable();
            names.dedup();
            names.into()
        })
    }

    /// The variables that closures created in arrow `e` assign.
    pub(crate) fn closure_assigned(&mut self, e: &ast::Expr) -> FxHashMap<String, Span> {
        kept(&mut self.assigned, e, || {
            let assigned = match arrow_parts(e) {
                Some((defaults, Some(stmts), _)) => assigned_by_closures(stmts, defaults),
                Some((defaults, None, value)) => assigned_by_closures(&[], defaults.chain(value)),
                None => Default::default(),
            };
            super::closure_assigned::owned(assigned)
        })
    }

    /// Where each variable arrow `e` assigns is last assigned.
    pub(crate) fn assignment_ends(&mut self, e: &ast::Expr) -> FxHashMap<String, u32> {
        kept(&mut self.ends, e, || match arrow_parts(e) {
            Some((defaults, Some(stmts), _)) => assignment_ends(stmts, defaults),
            Some((defaults, None, value)) => assignment_ends(&[], defaults.chain(value)),
            None => FxHashMap::default(),
        })
    }

    /// Does a closure in `init`, the initializer of `const name`, name it?
    pub(crate) fn late_const(&mut self, init: &ast::Expr, name: &str) -> bool {
        let scan = || super::late_consts::closure_uses(init, name);
        match key(init) {
            Some(k) => *self.late.entry((k, name.to_string())).or_insert_with(scan),
            None => scan(),
        }
    }
}

/// The parameter defaults of arrow `e`, and its body's statements or its expression body.
fn arrow_parts(
    e: &ast::Expr,
) -> Option<(
    impl Iterator<Item = &ast::Expr> + '_,
    Option<&[ast::Stmt]>,
    Option<&ast::Expr>,
)> {
    let ast::ExprKind::Arrow { params, body, .. } = &e.kind else {
        return None;
    };
    let defaults = params.iter().filter_map(|p| p.default.as_ref());
    Some(match body {
        ast::ArrowBody::Block(b) => (defaults, Some(&b.stmts[..]), None),
        ast::ArrowBody::Expr(x) => (defaults, None, Some(&**x)),
    })
}

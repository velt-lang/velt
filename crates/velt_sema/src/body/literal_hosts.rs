//! Bodies that may hold object literals implementing interfaces (`expr/iface_object.rs`), checked
//! before error types are read.
//!
//! Such a literal adds an implementation of its interface while its body is checked, and what
//! its methods throw is part of what a call through the interface throws. A body that catches
//! those errors (`catch (e)` around `x.read()`) reads them from the implementations known when it
//! is checked, so the bodies that may add one are checked first, whatever the declaration order:
//! before the other bodies (`driver::check_bodies`), and on demand when an error type is read
//! earlier (`Ctx::throw_groups`). Which bodies may is read from the syntax: those with an object
//! literal holding a method, a getter or setter, or a value that may be a function.
//!
//! A literal in a body still being checked (the catching function holds one too, or its method
//! calls the catching function) is not an implementation yet; while its methods' error types are
//! computed, what they throw counts for their interface slots (`Ctx::provisional_slots`).

use velt_syntax::ast;
use velt_syntax::visit::{walk_fn, Visit};

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnSource};
use crate::hir::DefId;

#[derive(Default)]
pub(crate) struct LiteralHosts {
    /// The bodies not yet made sure of, in checking order.
    pending: std::collections::VecDeque<DefId>,
}

/// Records the function bodies that may hold such literals (before bodies are checked).
pub(crate) fn find(cx: &mut Ctx) {
    let mut pending = std::collections::VecDeque::new();
    for &d in &cx.fn_defs {
        let Some(DefInfo::Fn(f)) = cx.info.get(d.0 as usize) else {
            continue;
        };
        if let Some(FnSource::Decl(decl)) = f.source {
            let mut v = Finder(false);
            walk_fn(decl, &mut v);
            if v.0 {
                pending.push_back(d);
            }
        }
    }
    cx.literal_hosts.pending = pending;
}

/// Checks every recorded body not checked yet. A body checked here that reads error types
/// itself calls this again, which checks the bodies after it first; bodies being checked are
/// left to finish (what their literals' methods throw is known meanwhile from
/// `Ctx::provisional_slots`).
pub(crate) fn ensure_all(cx: &mut Ctx) {
    while let Some(d) = cx.literal_hosts.pending.pop_front() {
        super::ensure_body(cx, d);
    }
}

/// Finds an object literal that may implement an interface with methods: one with a member
/// that may hold a function (a method, an accessor, a function value, or a name or call that
/// may give one, `{ read: boom }`, `{ read }`).
struct Finder(bool);

impl<'a> Visit<'a> for Finder {
    fn expr(&mut self, e: &'a ast::Expr) {
        let ast::ExprKind::Object(props) = &e.kind else {
            return;
        };
        self.0 |= props.iter().any(|p| match p {
            ast::ObjectProp::KeyValue(_, v) => !matches!(
                &v.kind,
                ast::ExprKind::Lit(_)
                    | ast::ExprKind::Template { .. }
                    | ast::ExprKind::Array(_)
                    | ast::ExprKind::Object(_)
                    | ast::ExprKind::StructLit { .. }
                    | ast::ExprKind::Unary { .. }
                    | ast::ExprKind::Binary { .. }
                    | ast::ExprKind::Update { .. }
                    | ast::ExprKind::New { .. }
                    | ast::ExprKind::InstanceOf { .. }
                    | ast::ExprKind::Jsx(_)
            ),
            ast::ObjectProp::Shorthand(_) | ast::ObjectProp::Method(_) => true,
            ast::ObjectProp::Spread(_) => false,
        });
    }
}

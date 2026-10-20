//! Bodies that may hold object literals implementing interfaces (`expr/iface_object.rs`), checked
//! before error types are read.
//!
//! Such a literal adds an implementation of its interface while its body is checked, and what
//! its methods throw is part of what a call through the interface throws. A body that catches
//! those errors (`catch (e)` around `x.read()`) reads them from the implementations known when it
//! is checked, so the bodies that may add one are checked first, whatever the declaration order:
//! before the other bodies (`driver::check_bodies`), and on demand when what a call through an
//! interface method throws is read earlier (`crate::throws`): then only the bodies with a literal
//! naming that method are checked ([`ensure_named`]), so a body is not checked inside an
//! unrelated one (it would find that one in progress, as if it were a recursion). Which bodies
//! may hold one is read from the syntax: those with an object literal holding a method, a
//! getter or setter, or a value that may be a function, recorded with the literals' member names.
//!
//! A literal in a body still being checked (the catching function holds one too, or its method
//! calls the catching function) is not an implementation yet; while its methods' error types are
//! computed, what they throw counts for their interface slots (`Ctx::provisional_slots`).

use rustc_hash::FxHashSet;

use velt_syntax::ast;
use velt_syntax::visit::{walk_fn, Visit};

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnSource};
use crate::hir::DefId;

#[derive(Default)]
pub(crate) struct LiteralHosts {
    /// The bodies not yet made sure of, in checking order, with the member names of their
    /// object literals that may hold functions.
    pending: Vec<(DefId, FxHashSet<String>)>,
    /// Module constants being checked: their initializers may lead to function bodies that
    /// read the constant, so no queued body is checked meanwhile (it would find the constant
    /// in progress, a false cycle); the queue is drained after them.
    pub(crate) in_constants: u32,
}

/// Records the function bodies that may hold such literals (before bodies are checked).
pub(crate) fn find(cx: &mut Ctx) {
    let mut pending = vec![];
    for &d in &cx.fn_defs {
        let Some(DefInfo::Fn(f)) = cx.info.get(d.0 as usize) else {
            continue;
        };
        if let Some(FnSource::Decl(decl)) = f.source {
            let mut v = Finder::default();
            walk_fn(decl, &mut v);
            if !v.0.is_empty() {
                pending.push((d, v.0));
            }
        }
    }
    cx.literal_hosts.pending = pending;
}

/// Checks every recorded body not checked yet, in order.
pub(crate) fn ensure_all(cx: &mut Ctx) {
    if cx.literal_hosts.in_constants > 0 {
        return;
    }
    while !cx.literal_hosts.pending.is_empty() {
        let (d, _) = cx.literal_hosts.pending.remove(0);
        super::ensure_body(cx, d);
    }
}

/// Checks the recorded bodies not checked yet whose literals have a member `name` (what a call
/// through an interface method or field of that name throws is about to be read). Bodies being
/// checked are left to finish.
pub(crate) fn ensure_named(cx: &mut Ctx, name: &str) {
    if cx.literal_hosts.in_constants > 0 {
        return;
    }
    while let Some(i) = cx
        .literal_hosts
        .pending
        .iter()
        .position(|(_, names)| names.contains(name))
    {
        let (d, _) = cx.literal_hosts.pending.remove(i);
        super::ensure_body(cx, d);
    }
}

/// The member names of object literals that may implement an interface with methods: members
/// that may hold a function (a method, an accessor, a function value, or a name or call that
/// may give one, `{ read: boom }`, `{ read }`).
#[derive(Default)]
struct Finder(FxHashSet<String>);

impl<'a> Visit<'a> for Finder {
    fn expr(&mut self, e: &'a ast::Expr) {
        let ast::ExprKind::Object(props) = &e.kind else {
            return;
        };
        for p in props {
            let name = match p {
                ast::ObjectProp::KeyValue(k, v) => {
                    let plain = matches!(
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
                    );
                    (!plain).then_some(k)
                }
                ast::ObjectProp::Shorthand(k) => Some(k),
                ast::ObjectProp::Method(f) => Some(&f.sig.name),
                ast::ObjectProp::Spread(_) => None,
            };
            if let Some(k) = name {
                self.0.insert(k.name.clone());
            }
        }
    }
}

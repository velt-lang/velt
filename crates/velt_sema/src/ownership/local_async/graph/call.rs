//! Calls in the flow graph: a direct call passes its arguments into the callee's parameters and
//! takes its result; a call through a function value records an `Indirect` call, whose callee
//! and arguments get nodes of their own (`super::super::call_sites`); a dispatched call records
//! the methods it may reach.

use crate::hir::{Callee, Def, Expr, ExprKind as E};

use super::super::call_sites::Indirect;
use super::{Node, To, Walk, STORED};

impl Walk<'_, '_> {
    /// Is `f` a read of a variable holding an accessor that `Object.defineProperty` replaces
    /// (`crate::defined_accessors::is_accessor_cell`)?
    pub(super) fn accessor_cell(&self, mut f: &Expr) -> bool {
        loop {
            match &f.kind {
                E::UnwrapSome(x, _) | E::Cast(x) | E::UnwrapVariant { expr: x, .. } => f = x,
                E::Local(l, _) => {
                    let name = &self.f.body.locals[l.0 as usize].name;
                    return crate::defined_accessors::is_accessor_cell(name);
                }
                _ => return false,
            }
        }
    }

    pub(super) fn call(&mut self, e: &Expr, callee: &Callee, args: &[Expr], to: To) {
        match callee {
            Callee::Def(g, targs) => {
                if !targs.is_empty() {
                    let site = (self.d, targs.clone());
                    self.g.insts.entry(*g).or_default().push(site);
                }
                let direct = matches!(&self.cx.defs[g.0 as usize], Some(Def::Fn(gf)) if gf.params.len() >= args.len());
                self.pass_to_params(Some(*g), args, e.span);
                *self.g.held.direct.entry(*g).or_insert(true) &= direct;
                if direct {
                    let n = self.ret_node(*g);
                    self.connect(n, to);
                } else {
                    self.unknown(e, to);
                }
            }
            Callee::Indirect(f) => {
                let call = self.g.indirect.len() as u32;
                // Where the called value comes from, so that a closure that stays in locals is
                // matched only with the calls it may reach (`super::call_sites`).
                let callee = self.g.node(Node::Callee(call), f.ty);
                self.value(f, To::Node(callee, None));
                // A variable of `Object.defineProperty`'s rewrite holds only closures of this
                // function, which keep nothing as a function value might.
                let cell = self.accessor_cell(f);
                let mut nodes = vec![];
                for (i, a) in args.iter().enumerate() {
                    let n = self.g.node(Node::Arg(call, i as u32), a.ty);
                    self.value(a, To::Node(n, None));
                    // The callee runs on this task; what it keeps, it keeps in the heap.
                    if !cell {
                        self.g.seeds.push((n, STORED, None));
                    }
                    nodes.push(n);
                }
                self.g.indirect.push(Indirect {
                    ty: f.ty,
                    callee,
                    args: nodes,
                    span: e.span,
                    library: self.library,
                    only: cell.then_some(self.d),
                });
                if let To::Node(m, _) = to {
                    let callees = self.g.call_results.entry(m).or_default();
                    if !callees.contains(&callee) {
                        callees.push(callee);
                    }
                }
                self.unknown(e, to);
            }
            Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                // What the callee keeps, it keeps in the heap; a parameter of a method it may
                // reach that crosses makes the argument cross (`super::crossing_params`).
                let call = self.g.dispatched_calls.len() as u32;
                let mut nodes = vec![];
                for (i, a) in args.iter().enumerate() {
                    if i == 0 {
                        self.value(a, To::Drop);
                        nodes.push(None);
                        continue;
                    }
                    let n = self.g.node(Node::DArg(call, i as u32), a.ty);
                    self.value(a, To::Node(n, None));
                    self.g.seeds.push((n, STORED, None));
                    nodes.push(Some(n));
                }
                if let (
                    Callee::Dyn {
                        slot,
                        method_type_args,
                    },
                    Some(r),
                ) = (callee, args.first())
                {
                    (self.g.dyn_calls).call(self.cx, self.d, r.ty, *slot, method_type_args);
                }
                if let Callee::ParamMethod {
                    iface,
                    slot,
                    method_type_args,
                    ..
                } = callee
                {
                    (self.g.dyn_calls).bounded(*iface, self.d, *slot, method_type_args);
                }
                let targets =
                    super::super::unknown_callees::dispatch_targets(self.cx, callee, args);
                self.g
                    .dispatched_calls
                    .push((targets, nodes, e.span, self.library));
                self.unknown(e, to);
            }
            Callee::Intrinsic(i) => self.intrinsic(e, *i, args, to),
        }
    }
}

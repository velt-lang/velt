//! `const x = node.left` bound by reference (`crate::body::const_borrow`): the binding points
//! into `node`, so the rest of its block must not modify or move `node.left` or anything that
//! owns it (`node.left = …`, `node = …`, `node.reset()`), which could free what `x` points to.
//! Changing `x`'s own contents (`x.v = 1`) or unrelated fields (`node.right = …`) is fine.
//! `const me = this` names the object itself, which nothing can replace: only using `me`
//! together with `this` (`super::this_alias`) ends the borrow.
//! When the block does change it, `x` becomes a share of the value instead (semantics stage 2:
//! `x` keeps referring to the object, like in JS); only values that cannot be shared (promises)
//! report the conflict.

use velt_common::{Diagnostic, Span};

use crate::hir::{
    Block, Callee, Expr, ExprKind as E, LocalDef, PatKind, Stmt, StmtKind as S, TyKind, UseMode,
};
use crate::visit::{self, VisitMut};

use super::iteration::contains;
use super::this_alias;
use super::uses::{Access, Collector, Place, Use};

/// Check every by-reference `const` in function body `body` (nested blocks included).
pub(super) fn check_body(
    col: &Collector,
    locals: &[LocalDef],
    shared: &[bool],
    body: &mut Block,
    out: &mut Vec<Diagnostic>,
) {
    let mut v = Lets {
        col,
        locals,
        shared,
        out,
    };
    v.scan(body);
    visit::block(body, &mut v);
}

/// Visits every block of a body once: the statements of each are checked in `scan`.
struct Lets<'c, 'a, 'm> {
    col: &'c Collector<'a, 'm>,
    locals: &'c [LocalDef],
    /// Per local: is its value shared (`Ctx::is_shared_value`)?
    shared: &'c [bool],
    out: &'c mut Vec<Diagnostic>,
}

impl Lets<'_, '_, '_> {
    fn scan(&mut self, b: &mut Block) {
        for i in 0..b.stmts.len() {
            let (head, rest) = b.stmts.split_at_mut(i + 1);
            let value = b.value.as_deref_mut();
            if let Some((d, by_value)) = check_let(self.col, self.locals, &mut head[i], rest, value)
            {
                match share_binding(&mut head[i], self.shared, by_value) {
                    true => {}
                    false => self.out.push(d),
                }
            }
        }
    }
}

impl VisitMut for Lets<'_, '_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            S::If { then, els, .. } => {
                self.scan(then);
                if let Some(b) = els {
                    self.scan(b);
                }
            }
            S::While { body, .. } | S::ForOf { body, .. } | S::Block(body) => self.scan(body),
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.scan(body);
                if let Some((_, b)) = catch {
                    self.scan(b);
                }
                if let Some(b) = finally {
                    self.scan(b);
                }
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        if let E::Block(b) = &mut e.kind {
            self.scan(b);
        }
    }
}

/// The first change of the place `s` (a by-reference `const`) borrows, in the statements after it,
/// and whether only a function value can make it (`changed_by_function_value`).
fn check_let(
    col: &Collector,
    locals: &[LocalDef],
    s: &mut Stmt,
    rest: &mut [Stmt],
    value: Option<&mut Expr>,
) -> Option<(Diagnostic, bool)> {
    let S::LetPat { pat, init } = &s.kind else {
        return None;
    };
    let PatKind::Binding(l, UseMode::Borrow) = pat.kind else {
        return None;
    };
    // `const c = a.load()` (`borrowed_load`) refers into the `Atomic`: its place is the
    // receiver's, which dropping or replacing would release.
    let (place, _) = col
        .place_of(init)
        .or_else(|| col.place_of(load_receiver(init)?))?;
    let at = init.span;
    let text = col.text(init);
    let mut uses = vec![];
    if let Some(this) = this_local(col, init) {
        return this_alias::conflict(col, l, this, rest, value).map(|span| {
            let name = &col.locals[l.0 as usize].name;
            let d = changed_while_borrowed("this", Access::Unique, span, name, "this", at);
            (d, false)
        });
    }
    let mut value = value;
    col.nested_stmts(rest, value.as_deref_mut(), &mut uses);
    let changes = |u: &Use| u.access != Access::Shared && contains(&u.place, &place);
    let by_value = !uses.iter().any(changes);
    if let Some(u) = changed_by_function_value(col, rest, value, &place) {
        uses.push(u);
    }
    let hit = uses.into_iter().find(changes)?;
    let name = &locals[l.0 as usize].name;
    let d = changed_while_borrowed(&hit.text, hit.access, hit.span, name, &text, at);
    Some((d, by_value))
}

/// The receiver of `init` when it is an `Atomic`'s `load` (bound by reference only for an
/// `Atomic` of a reference).
fn load_receiver(init: &Expr) -> Option<&Expr> {
    match &init.kind {
        E::Call {
            callee: crate::hir::Callee::Intrinsic(crate::hir::Intrinsic::AtomicLoad),
            args,
        } => args.first(),
        _ => None,
    }
}

/// `this` when `init` is the method's own `this` (`const me = this`, `crate::body::const_borrow`;
/// no other local can be named `this`).
fn this_local(col: &Collector, init: &Expr) -> Option<crate::hir::LocalId> {
    match init.kind {
        E::Local(l, _) if col.locals[l.0 as usize].name == "this" => Some(l),
        _ => None,
    }
}

/// Turn the by-reference `const` `s` into an owned share of the place it referred to; false
/// when its value cannot be shared. Changed only `by_value`, it stays bound by reference to a share
/// of the place, and lowering, which knows what is written, may keep the plain reference.
fn share_binding(s: &mut Stmt, shared: &[bool], by_value: bool) -> bool {
    let S::LetPat { pat, init } = &mut s.kind else {
        return false;
    };
    let PatKind::Binding(l, UseMode::Borrow) = pat.kind else {
        return false;
    };
    if !shared[l.0 as usize] {
        return false;
    }
    if by_value {
        super::super::soft::make_share(init);
        return true;
    }
    let mut init = init.clone();
    super::super::soft::make_share(&mut init);
    s.kind = S::Let {
        local: l,
        init: Some(init),
    };
    true
}

fn changed_while_borrowed(
    what: &str,
    access: Access,
    span: Span,
    name: &str,
    text: &str,
    at: Span,
) -> Diagnostic {
    let verb = if access == Access::Move {
        "move"
    } else {
        "modify"
    };
    Diagnostic::error(
        format!("cannot {verb} `{what}` while `{name}` refers to `{text}`"),
        span,
    )
    .with_label(at, format!("`{name}` refers to `{text}` from here"))
    .with_note(format!(
        "`{name}` is the value stored in `{text}`, not a copy; declare `{name}` after the change, or copy the value with `.clone()`"
    ))
}

/// A call through a function value, or to a function, constructor or method chosen at run time
/// that passes the param on to one (`crate::value_params`), given what owns `place` (`const x = a.b; g(a)`): its params read as
/// borrows however the function value changes them, as it may replace `a.b`.
fn changed_by_function_value(
    col: &Collector,
    rest: &mut [Stmt],
    value: Option<&mut Expr>,
    place: &Place,
) -> Option<Use> {
    let mut v = ValueCalls {
        col,
        place,
        equal: false,
        hit: None,
    };
    for s in rest {
        visit::stmt(s, &mut v);
    }
    if let Some(e) = value {
        visit::expr(e, &mut v);
    }
    v.hit
}

/// `crate::value_params::Calls` while the program is being checked.
pub(super) struct Checking<'c, 'm>(pub &'c crate::ctx::Ctx<'m>);

impl<'c> crate::value_params::Calls<'c> for Checking<'c, '_> {
    fn method_name(&self, callee: &Callee, args: &[Expr]) -> Option<&'c str> {
        method_name(self.0, callee, args)
    }

    fn plain(&self, t: crate::hir::TyId) -> bool {
        crate::value_params::is_plain(self.0.ty.kind(t))
    }

    fn ctor(&self, class: crate::hir::DefId) -> Option<crate::hir::DefId> {
        self.0.adt(class)?.ctor
    }
}

/// The name of the method a call chooses at run time (`crate::value_params`), when the receiver's
/// type says.
fn method_name<'c>(cx: &'c crate::ctx::Ctx, callee: &Callee, args: &[Expr]) -> Option<&'c str> {
    let iface = |d, slot: &u32| {
        let m = cx.iface(d)?.methods.get(*slot as usize)?;
        Some(m.name.as_str())
    };
    match callee {
        Callee::Virtual { slot } => {
            let (class, _) = cx.class_of(args.first()?.ty)?;
            let m = *cx.adt(class)?.vtable.get(*slot as usize)?;
            Some(cx.try_fn(m)?.name.as_str())
        }
        Callee::Dyn { slot, .. } => match cx.ty.kind(args.first()?.ty) {
            TyKind::Dyn(d, _) => iface(*d, slot),
            _ => None,
        },
        Callee::ParamMethod { iface: d, slot, .. } => iface(*d, slot),
        _ => None,
    }
}

/// The same for the body of a loop over `place`, or (`equal`) given `place` itself too: a function
/// value given the array may empty it.
pub(super) fn handed_to_function_value(
    col: &Collector,
    body: &mut Block,
    place: &Place,
    equal: bool,
) -> Option<Use> {
    let mut v = ValueCalls {
        col,
        place,
        equal,
        hit: None,
    };
    visit::block(body, &mut v);
    v.hit
}

struct ValueCalls<'c, 'a, 'm> {
    col: &'c Collector<'a, 'm>,
    place: &'c Place,
    equal: bool,
    hit: Option<Use>,
}

impl ValueCalls<'_, '_, '_> {
    /// The arguments `bits` says.
    fn arguments(&mut self, args: &[Expr], bits: u64) {
        for (i, a) in args.iter().enumerate().take(64) {
            if bits >> i & 1 == 1 {
                self.argument(a);
            }
        }
    }

    /// Argument `a` of such a call, when it is what owns the place.
    fn argument(&mut self, a: &Expr) {
        let mut x = a;
        while let E::Upcast(y) | E::WrapSome(y) | E::ToDyn { expr: y, .. } | E::Cast(y) = &x.kind {
            x = y;
        }
        let Some((p, _)) = self.col.place_of(x) else {
            return;
        };
        let owner =
            (self.equal || p.proj.len() < self.place.proj.len()) && contains(&p, self.place);
        if owner && self.hit.is_none() {
            self.hit = Some(Use {
                place: p,
                access: Access::Unique,
                span: a.span,
                text: self.col.text(x),
                captured: false,
                capture: None,
                identity: false,
                inside: false,
            });
        }
    }
}

impl VisitMut for ValueCalls<'_, '_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        if let E::New { def, args, .. } = &e.kind {
            let bits = self.col.value_params.of_new(*def, args.len());
            return self.arguments(args, bits);
        }
        let E::Call { callee, args } = &e.kind else {
            return;
        };
        match callee {
            Callee::Indirect(_) => args.iter().for_each(|a| self.argument(a)),
            Callee::Def(d, _) => {
                let bits = self.col.value_params.of_args(*d, args.len());
                self.arguments(args, bits);
            }
            Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                let name = method_name(self.col.cx, callee, args);
                let bits = self.col.value_params.of_method(name, args.len());
                self.arguments(args, bits);
            }
            _ => {}
        }
    }
}

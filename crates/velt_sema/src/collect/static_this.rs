//! `this` and `super` in static methods (TypeScript): `this` is the class the method was called
//! on, so `B.hello()` running `A`'s `static hello() { return this.kind(); }` calls `B.kind`.
//!
//! A static method whose body uses `this` or `super` gets a copy per subclass of its class: the
//! same body checked with `this` being that subclass. `B.hello()` calls the copy for `B`, and
//! calls inside the body (`this.kind()`, `super.f()`) are resolved statically in each copy, so
//! there is no dispatch at run time. The copies are made here, before bodies are checked, for
//! every class below the declaring one: a copy is needed wherever a subclass's static runs an
//! inherited one, through `B.f()` or a chain of `super.f()` calls.

use std::collections::HashMap;

use velt_syntax::ast;
use velt_syntax::visit::{walk_fn, Visit};

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnSource};
use crate::hir::{AdtKind, DefId, TyKind};

pub(crate) fn copy_statics(cx: &mut Ctx) {
    let classes: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|&d| cx.adt(d).is_some_and(|a| a.kind == AdtKind::Class))
        .collect();
    // The statics using `this` or `super`, per declaring class.
    let mut users: HashMap<DefId, Vec<DefId>> = HashMap::new();
    for &c in &classes {
        let mut own: Vec<DefId> = cx
            .adt(c)
            .expect("ICE: class")
            .methods
            .values()
            .filter(|m| m.is_static)
            .map(|m| m.def)
            .collect();
        for x in &cx.extensions {
            if matches!(cx.ty.kind(x.target), TyKind::Adt(t, _) if *t == c) {
                own.extend(x.methods.values().filter(|m| m.is_static).map(|m| m.def));
            }
        }
        own.sort_unstable_by_key(|d| d.0);
        own.retain(|&m| uses_this(cx, m));
        for &m in &own {
            cx.static_this.insert(m, c);
        }
        if !own.is_empty() {
            users.insert(c, own);
        }
    }
    if users.is_empty() {
        return;
    }
    for &c in &classes {
        for ancestor in ancestors(cx, c) {
            for &m in users.get(&ancestor).into_iter().flatten() {
                let copy = copy_of(cx, m, c);
                cx.static_this.insert(copy, c);
                cx.static_copies.insert((m, c), copy);
                cx.static_copy_of.insert(copy, m);
            }
        }
    }
}

/// Does static method `m`'s body mention `this` or `super` (also in nested functions, which
/// only costs an unneeded copy)?
fn uses_this(cx: &Ctx, m: DefId) -> bool {
    struct Finder(bool);
    impl<'a> Visit<'a> for Finder {
        fn expr(&mut self, e: &'a ast::Expr) {
            self.0 |= matches!(e.kind, ast::ExprKind::This | ast::ExprKind::Super);
        }
    }
    let Some(FnSource::Decl(decl)) = cx.fn_info(m).source else {
        return false;
    };
    let mut f = Finder(false);
    walk_fn(decl, &mut f);
    f.0
}

/// The classes above class `c`, nearest first.
fn ancestors(cx: &Ctx, c: DefId) -> Vec<DefId> {
    let mut out = vec![];
    let mut cur = c;
    while let Some((b, _)) = cx
        .adt(cur)
        .and_then(|a| a.base)
        .and_then(|b| cx.class_of(b))
    {
        if b == c || out.contains(&b) {
            break;
        }
        out.push(b);
        cur = b;
    }
    out
}

/// A copy of static method `m` whose `this` is class `c`.
fn copy_of(cx: &mut Ctx, m: DefId, c: DefId) -> DefId {
    let mut info = cx.fn_info(m).clone();
    let class = cx.adt(c).expect("ICE: class").qual_name.clone();
    info.name = format!("{}#{class}", info.name);
    let span = cx.def_spans[m.0 as usize];
    cx.alloc_def(span, DefInfo::Fn(Box::new(info)))
}

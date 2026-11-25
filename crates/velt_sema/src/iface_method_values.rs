//! A function stored as an object-literal property that it isn't written in (`{ setKey:
//! handle.setKey }`, sigx's async engine, a variable, a call's result). Node calls it with the
//! literal as `this` (`o.setKey()`), while Velt keeps the object a method read as a value was
//! read from (#746), so the two differ for a method that reads `this`. Where the value came from
//! isn't followed. A function value not written in the literal (`body/expr/literal_this.rs`
//! `stored_value`) is refused, once every body is checked, when a method that may read `this`
//! has:
//!
//! - the property's name;
//! - the name of a method the value reads directly (`{ run: g.greet }`);
//! - the value's function type (its parameter and result types), when the value isn't a direct
//!   read of a method (whose name says what it is), which covers a method renamed through a
//!   variable or a parameter (`const f = g.greet; { run: f }`).
//!
//! A value of `any` or of a type parameter, which may hold a function, and a function field read
//! (`b.f`), are refused by the property's name only. A method that may read `this`:
//!
//! - a method of an object literal that uses `this` (`MethodValues::this_slots`);
//! - a class's method (static or not) whose body uses `this` or `super`;
//! - an interface's default body, when it uses them.
//!
//! Std's own interfaces and classes don't count: their method names (`get`, `add`, `push`) are
//! everywhere, and a std method stored this way throws in Node (an incompatible receiver)
//! rather than giving another result.
//!
//! Any interface's or class's method counts: an adapter fills an interface's slot with the
//! method of the value it adapts, and a subclass's override is called through its base class's
//! slot. A function written in the literal (an arrow, a function expression, a method), a
//! function declared by name and a `const` initialized with an arrow have no `this` of their
//! own. A method value that passes through a function's result or a field and is stored under
//! another name and type is not followed (#3650).
//!
//! The fact is the whole program's: std's stage of a separate check (`crate::units`) and a kept
//! check's reload (`crate::kept_reload`) keep the reads for the check that sees every body.

use rustc_hash::FxHashSet;
use velt_common::Span;

use crate::collect::static_this::uses_this;
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, TyId};

/// What this check gathers while bodies are checked (`ObjectImpls::method_values`).
#[derive(Clone, Debug, Default)]
pub(crate) struct MethodValues {
    /// The interface slots an object literal fills with a method reading `this`.
    pub this_slots: FxHashSet<(DefId, u32)>,
    /// The functions stored as object-literal properties they aren't written in.
    pub reads: Vec<StoredRead>,
}

/// A function not written in an object literal, stored as its property `name`
/// (`body/expr/literal_this.rs` `stored_value`).
#[derive(Clone, Debug)]
pub(crate) struct StoredRead {
    pub name: String,
    /// The value when it is a path (`g.greet`, `f`), for the fix the message suggests.
    pub read: Option<String>,
    pub span: Span,
    /// The methods the value reads directly (`{ run: g.greet }`): checked by their names too.
    pub methods: Vec<String>,
    /// The value's parameter and result types, when it is a function: checked against the
    /// methods reading `this`.
    pub sig: Option<(Vec<TyId>, TyId)>,
    /// The value is an `any` or a type parameter's, which may hold a function: checked by the
    /// property's name.
    pub dynamic: bool,
}

/// Report the functions stored where a method reading `this` may be (module docs).
pub(crate) fn check(cx: &mut Ctx) {
    // std's stage alone sees part of the program: its unit keeps the reads for the app's stage.
    if cx.units_stage == crate::units::Stage::Std {
        return;
    }
    let reads = std::mem::take(&mut cx.object_impls.method_values.reads);
    if reads.is_empty() {
        return;
    }
    let (names, sigs) = this_methods(cx);
    if names.is_empty() {
        return;
    }
    let mut seen = FxHashSet::default();
    for r in reads {
        if !seen.insert((r.span.file, r.span.lo, r.span.hi)) {
            continue;
        }
        let named = std::iter::once(&r.name)
            .chain(&r.methods)
            .find(|n| names.contains(n.as_str()));
        // The first method (in source order) of the value's function type.
        let typed = match &r.sig {
            Some(sig) => sigs.iter().find(|(_, _, s)| s == sig).map(|(n, _, _)| n),
            None => None,
        };
        let why = match (named, typed, r.dynamic) {
            (Some(n), ..) => crate::body::expr::StoredWhy::Named(n),
            (None, Some(n), _) => crate::body::expr::StoredWhy::Typed(n),
            // A value of `any` or a type parameter's is checked by its name only: whatever its
            // name, it would refuse every generic or dynamic value of a literal.
            (None, None, _) => continue,
        };
        let d = crate::body::expr::stored_method_error(&r.name, why, r.read.as_deref(), r.span);
        cx.error(d);
    }
}

/// A function signature: parameter and result types.
type Sig = (Vec<TyId>, TyId);

/// The names of the methods that may read `this` (module docs), and the parameter and result
/// types of each that isn't generic, with its name and place, in source order. Any interface's
/// counts, not only the one a value was read from: an adapter fills one interface's slot with
/// another interface's method, and a class's subclasses implement its interfaces' methods too.
fn this_methods(cx: &Ctx) -> (FxHashSet<String>, Vec<(String, Span, Sig)>) {
    let mut names = FxHashSet::default();
    let mut sigs = vec![];
    let params = |ps: &[crate::defs::ParamSig]| ps.iter().map(|p| p.ty).collect::<Vec<_>>();
    for &(d, slot) in &cx.object_impls.method_values.this_slots {
        if let Some(m) = cx.iface(d).and_then(|i| i.methods.get(slot as usize)) {
            names.insert(m.name.clone());
            if m.generics.names.is_empty() {
                sigs.push((m.name.clone(), m.span, (params(&m.params), m.ret)));
            }
        }
    }
    // Std's own types don't count (module docs).
    let std = |module: usize| cx.scopes[module].is_std;
    for info in &cx.info {
        match info {
            DefInfo::Iface(i) if !std(i.module) => {
                for m in &i.methods {
                    if m.default.is_some_and(|d| uses_this(cx, d)) {
                        names.insert(m.name.clone());
                        if m.generics.names.is_empty() {
                            sigs.push((m.name.clone(), m.span, (params(&m.params), m.ret)));
                        }
                    }
                }
            }
            DefInfo::Adt(a) if !std(a.module) => {
                for (name, m) in &a.methods {
                    if !uses_this(cx, m.def) {
                        continue;
                    }
                    names.insert(name.clone());
                    if let DefInfo::Fn(f) = &cx.info[m.def.0 as usize] {
                        if f.generics.names.is_empty() {
                            sigs.push((name.clone(), f.span, (params(&f.params), f.ret)));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // Source order, so the method a message names is the same on every machine.
    sigs.sort_by(|a, b| (a.1.file, a.1.lo, &a.0).cmp(&(b.1.file, b.1.lo, &b.0)));
    (names, sigs)
}

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::structs! {
    MethodValues { this_slots, reads }
    StoredRead { name, read, span, methods, sig, dynamic }
}

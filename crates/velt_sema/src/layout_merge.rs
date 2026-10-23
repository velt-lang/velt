//! Shared layouts of extended object types (`collect::extended_layouts`), made once the program
//! is checked: for each family the program converts in, the base's object type (its anonymous
//! twin) gets the extensions' optional fields, and the extensions' twins become that type.
//!
//! Each object literal of a type of that layout is given the added fields, absent
//! (`Intrinsic::FieldAbsent`), and each field read, write and pattern of such a type is renumbered
//! to the widened field list. The added fields come after the base's, each extension's in its
//! order, so a type's own fields keep their order (and the literal its evaluation order). Runs
//! after `finalize::build_defs`, before `crate::readonly::erase` replaces the types by their
//! twins.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::FieldInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, FieldDef, Intrinsic, Pat, PatKind};
use crate::hir::{TyId, TyKind};
use crate::object_copies::Seen;
use crate::visit::{self, VisitMut};

/// Widen the layouts of the families in use (module docs).
pub(crate) fn merge(cx: &mut Ctx) {
    let used: Vec<DefId> = (cx.objects.layouts.families.iter())
        .filter(|(_, f)| f.used)
        .map(|(d, _)| *d)
        .collect();
    if used.is_empty() {
        return;
    }
    let mut remap: FxHashMap<DefId, Remap> = FxHashMap::default();
    for base in used {
        let redirect: FxHashMap<DefId, DefId> = (cx.objects.layouts.redirect.iter())
            .filter(|(_, b)| **b == base)
            .map(|(e, b)| (*e, *b))
            .collect();
        crate::collect::extended_layouts::redirect_twins(&mut cx.readonly_twins, &redirect);
        let added = cx.objects.layouts.families[&base].added.clone();
        let mut names = field_names(cx, base);
        let mut defs: Vec<FieldDef> = vec![];
        let mut infos = vec![];
        for f in added {
            if names.contains(&f.name) {
                continue;
            }
            // The field as its extension's twin lays it out (presence flag, default).
            let def = redirect.keys().find_map(|e| match &cx.defs[e.0 as usize] {
                Some(Def::Adt(a)) => a.fields.iter().find(|g| g.name == f.name).cloned(),
                _ => None,
            });
            let Some(def) = def else {
                continue;
            };
            names.push(f.name.clone());
            defs.push(def);
            infos.push(f);
        }
        let sources: Vec<DefId> = (cx.readonly_twins.iter())
            .filter(|(_, t)| t.0 == base)
            .map(|(d, _)| *d)
            .chain([base])
            .collect();
        // A generic object type of one of these shapes (`{ type: T0; dom: T1 }`) is the same
        // layout at concrete arguments (`Ctx::canon`): it gets the fields too, and so does each
        // object type of its field names at other types (`{ type: number; dom: string }`),
        // which lowering lays out as its instance.
        let shapes: Vec<Vec<(String, bool)>> = sources.iter().map(|s| shape(cx, *s)).collect();
        let alike = |cx: &Ctx, d: DefId, generic: bool| {
            let anon = cx.adt(d).is_some_and(|a| {
                a.kind == crate::hir::AdtKind::Anon && (a.generics.len() != 0) == generic
            });
            anon && !cx.readonly_twins.contains_key(&d) && shapes.contains(&shape(cx, d))
        };
        let mut generic: Vec<DefId> = (0..cx.info.len() as u32)
            .map(DefId)
            .filter(|d| alike(cx, *d, true))
            .collect();
        if !generic.is_empty() {
            generic.extend(
                (0..cx.info.len() as u32)
                    .map(DefId)
                    .filter(|d| *d != base && !sources.contains(d) && alike(cx, *d, false)),
            );
        }
        let old: Vec<(DefId, Vec<String>)> = (sources.iter().chain(&generic))
            .map(|s| (*s, field_names(cx, *s)))
            .collect();
        let full_defs: Vec<FieldDef> = match &cx.defs[base.0 as usize] {
            Some(Def::Adt(a)) => a.fields.iter().cloned().chain(defs).collect(),
            _ => continue,
        };
        let full_infos: Vec<FieldInfo> = (cx.adt(base).map_or(vec![], |a| a.fields.clone()))
            .into_iter()
            .chain(infos)
            .collect();
        for &g in generic.iter().chain([&base]) {
            // Its own fields as they are, the others as the base's layout has them.
            let mine: Vec<FieldDef> = match &cx.defs[g.0 as usize] {
                Some(Def::Adt(a)) => a.fields.clone(),
                _ => continue,
            };
            let my_infos = cx.adt(g).map_or(vec![], |a| a.fields.clone());
            let fields: Vec<FieldDef> = (names.iter().enumerate())
                .map(|(i, n)| mine.iter().find(|f| f.name == *n).unwrap_or(&full_defs[i]))
                .cloned()
                .collect();
            let field_infos: Vec<FieldInfo> = (names.iter().enumerate())
                .map(|(i, n)| {
                    my_infos
                        .iter()
                        .find(|f| f.name == *n)
                        .unwrap_or(&full_infos[i])
                })
                .cloned()
                .collect();
            if let Some(Def::Adt(a)) = &mut cx.defs[g.0 as usize] {
                a.fields = fields;
            }
            cx.adt_mut(g).fields = field_infos;
        }
        // A generic shape's readonly views read its fields.
        let views: Vec<(DefId, DefId)> = (cx.readonly_twins.iter())
            .filter(|(_, t)| generic.contains(&t.0))
            .map(|(d, t)| (*d, t.0))
            .collect();
        for (s, own) in old {
            let target = if generic.contains(&s) { s } else { base };
            remap.insert(s, Remap::new(target, &own, &names, target == base));
        }
        for (view, g) in views {
            let own = field_names(cx, view);
            remap.insert(view, Remap::new(g, &own, &names, false));
        }
    }
    let field_tys: FxHashMap<DefId, Vec<TyId>> = (remap.values())
        .map(|r| r.layout)
        .filter_map(|l| match &cx.defs[l.0 as usize] {
            Some(Def::Adt(a)) => Some((l, a.fields.iter().map(|f| f.ty).collect())),
            _ => None,
        })
        .collect();
    let mut r = Renumber {
        remap: &remap,
        field_tys: &field_tys,
        ty: &cx.ty,
    };
    for def in cx.defs.iter_mut().flatten() {
        match def {
            Def::Fn(f) => visit::block(&mut f.body.block, &mut r),
            Def::Global(g) => visit::expr(&mut g.init, &mut r),
            Def::Adt(a) => {
                for d in a.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    visit::expr(d, &mut r);
                }
            }
            _ => {}
        }
    }
}

/// Where an object type's fields are in its widened layout.
struct Remap {
    /// The type its literals build: the base's layout, or a generic shape itself.
    layout: DefId,
    /// Field `i` of the type is field `at[i]` of `layout`.
    at: Vec<u32>,
    /// Its literals build `layout` (a type the base's layout replaces before lowering).
    retarget: bool,
}

impl Remap {
    fn new(layout: DefId, own: &[String], names: &[String], retarget: bool) -> Remap {
        let at = (own.iter())
            .map(|n| (names.iter().position(|m| m == n)).map_or(u32::MAX, |i| i as u32))
            .collect();
        Remap {
            layout,
            at,
            retarget,
        }
    }
}

/// The field names and optional flags of object type `d`, in order.
fn shape(cx: &Ctx, d: DefId) -> Vec<(String, bool)> {
    cx.adt(d).map_or(vec![], |a| {
        (a.fields.iter())
            .map(|f| (f.name.clone(), f.optional))
            .collect()
    })
}

/// The field names of object type `d`, in order.
fn field_names(cx: &Ctx, d: DefId) -> Vec<String> {
    match &cx.defs[d.0 as usize] {
        Some(Def::Adt(a)) => a.fields.iter().map(|f| f.name.clone()).collect(),
        _ => cx.adt(d).map_or(vec![], |a| {
            a.fields.iter().map(|f| f.name.clone()).collect()
        }),
    }
}

struct Renumber<'a> {
    remap: &'a FxHashMap<DefId, Remap>,
    field_tys: &'a FxHashMap<DefId, Vec<TyId>>,
    ty: &'a crate::types::Types,
}

impl Renumber<'_> {
    /// The object type `t` is (also as a `Shared` cell's value), when its layout is widened.
    fn layout_of(&self, t: TyId) -> Option<&Remap> {
        let t = match self.ty.kind(t) {
            TyKind::Shared(x) => *x,
            _ => t,
        };
        match self.ty.kind(t) {
            TyKind::Adt(d, _) => self.remap.get(d),
            _ => None,
        }
    }
}

impl VisitMut for Renumber<'_> {
    fn expr(&mut self, e: &mut Expr) {
        let span = e.span;
        match &mut e.kind {
            E::Field { base, index, .. } => {
                if let Some(r) = self.layout_of(base.ty) {
                    *index = r.at.get(*index as usize).copied().unwrap_or(*index);
                }
            }
            E::AdtLit {
                def,
                type_args,
                fields,
                ..
            } => {
                let Some(r) = self.remap.get(def) else {
                    return;
                };
                let tys = &self.field_tys[&r.layout];
                let mut slots: Vec<Option<Expr>> = (0..tys.len()).map(|_| None).collect();
                for (i, f) in std::mem::take(fields).into_iter().enumerate() {
                    match r.at.get(i).and_then(|&k| slots.get_mut(k as usize)) {
                        Some(s) => *s = Some(f),
                        None => panic!("ICE: a field missing from its widened layout"),
                    }
                }
                *fields = (slots.into_iter().zip(tys))
                    .map(|(s, &ty)| {
                        s.unwrap_or_else(|| Expr {
                            kind: E::Call {
                                callee: Callee::Intrinsic(Intrinsic::FieldAbsent),
                                args: vec![],
                            },
                            ty,
                            span,
                        })
                    })
                    .collect();
                if r.retarget {
                    *def = r.layout;
                    type_args.clear();
                }
            }
            _ => {}
        }
    }

    fn pat(&mut self, p: &mut Pat) {
        let Some(r) = self.layout_of(p.ty) else {
            return;
        };
        if let PatKind::Adt { fields } = &mut p.kind {
            for (i, _) in fields.iter_mut() {
                *i = r.at.get(*i as usize).copied().unwrap_or(*i);
            }
        }
    }
}

/// Before the layouts are widened (the program checked, `Layouts::sealed`): report each
/// `Object.keys`, `for…in`, spread and `"k" in x` of a type of a widened layout that lacks a
/// field an extension adds and that the program may give a value ([`given_fields`]). Those list
/// the type's own fields; Node's object would have the given one too.
pub(crate) fn check_looks(cx: &mut Ctx) {
    if !cx.objects.layouts.families.values().any(|f| f.used) {
        return;
    }
    let looks: Vec<(TyId, velt_common::Span, Seen)> = (cx.object_copies.seen.iter())
        .filter(|s| matches!(s.2, Seen::Keys | Seen::Spread | Seen::In(_)))
        .cloned()
        .collect();
    let mut reported = vec![];
    let mut given: FxHashMap<DefId, Vec<String>> = FxHashMap::default();
    for (t, span, how) in looks {
        let t = cx.ty.opt_payload(t).unwrap_or(t);
        let TyKind::Adt(d, _) = *cx.ty.kind(t) else {
            continue;
        };
        let Some(base) = used_family(cx, d) else {
            continue;
        };
        if reported.contains(&span) {
            continue;
        }
        let own: Vec<String> = cx.adt(d).map_or(vec![], |a| {
            a.fields.iter().map(|f| f.name.clone()).collect()
        });
        let written = &*given.entry(base).or_insert_with(|| given_fields(cx, base));
        let missing = match how {
            Seen::In(k) => (cx.object_copies.in_keys.get(k as usize))
                .filter(|k| written.contains(k))
                .cloned(),
            _ => written.iter().find(|n| !own.contains(n)).cloned(),
        };
        let Some(field) = missing else {
            continue;
        };
        reported.push(span);
        let tn = cx.display(t);
        let what = match how {
            Seen::Keys => "lists the keys of",
            Seen::Spread => "spreads",
            _ => "tests a key of",
        };
        let ext = extension_with(cx, base, &field);
        let shown = cx.shown_name(&field);
        cx.error(
            velt_common::Diagnostic::error(
                format!(
                    "this {what} a value of type `{tn}`, which has its own fields only, but such objects can have `{shown}` (given through `{ext}`), and Node would show it here"
                ),
                span,
            )
            .with_note(format!(
                "a `{tn}` converts to `{ext}` as the same object, which has room for `{shown}`"
            ))
            .with_note(format!("look at the value as `{ext}` (`v as {ext}`)")),
        );
    }
}

/// The base layout of the family in use that object type `d` is of.
fn used_family(cx: &Ctx, d: DefId) -> Option<DefId> {
    let l = &cx.objects.layouts;
    let twin = cx.readonly_twins.get(&d).map_or(d, |t| t.0);
    let base = l.redirect.get(&twin).copied().unwrap_or(twin);
    l.families
        .get(&base)
        .is_some_and(|f| f.used)
        .then_some(base)
}

/// The fields the extensions in family `base` add that objects of the family may have: those
/// the program assigns (on a type of the family, or through `any`), those an object literal of
/// a type of the family gives a value (`{ ...v, _ns: true }` too), and all of them where the
/// family's objects may come from text or a dynamic value (`JSON.parse`, `x as VNode` of an
/// `any`), whose keys are not known.
fn given_fields(cx: &mut Ctx, base: DefId) -> Vec<String> {
    let added: Vec<String> = (cx.objects.layouts.families[&base].added.iter())
        .map(|f| f.name.clone())
        .collect();
    let mut scan = GivenScan::default();
    for def in cx.defs.iter_mut().flatten() {
        match def {
            Def::Fn(f) => visit::block(&mut f.body.block, &mut scan),
            Def::Global(g) => visit::expr(&mut g.init, &mut scan),
            _ => {}
        }
    }
    for info in cx.info.iter_mut() {
        match info {
            crate::defs::DefInfo::Global(g) => {
                if let Some(e) = &mut g.init {
                    visit::expr(e, &mut scan);
                }
            }
            crate::defs::DefInfo::Adt(a) => {
                for d in a.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    visit::expr(d, &mut scan);
                }
            }
            _ => {}
        }
    }
    let unknown = (cx.objects.layouts.parsed.iter().chain(&scan.dynamic))
        .any(|&t| mentions_family(cx, t, base));
    if unknown {
        return added;
    }
    let mut given = written_fields(cx, base, &added);
    for (d, present) in &scan.literals {
        if used_family(cx, *d) != Some(base) {
            continue;
        }
        let names: Vec<String> = cx.adt(*d).map_or(vec![], |a| {
            a.fields.iter().map(|f| f.name.clone()).collect()
        });
        for (n, p) in names.iter().zip(present) {
            if *p && added.contains(n) && !given.contains(n) {
                given.push(n.clone());
            }
        }
    }
    given
}

/// What [`given_fields`] looks for in the program's bodies.
#[derive(Default)]
struct GivenScan {
    /// Each object literal's type and which of its fields it gives a value.
    literals: Vec<(DefId, Vec<bool>)>,
    /// The types dynamic values are converted to (`Intrinsic::AnyTo`).
    dynamic: Vec<TyId>,
}

impl VisitMut for GivenScan {
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::AdtLit { def, fields, .. } => {
                let present = fields.iter().map(|f| !is_absent(f)).collect();
                self.literals.push((*def, present));
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::AnyTo),
                ..
            } => self.dynamic.push(e.ty),
            _ => {}
        }
    }
}

/// An object literal's value for a field it leaves out (`FieldAbsent`, `null`, the dynamic
/// `undefined`).
fn is_absent(e: &Expr) -> bool {
    match &e.kind {
        E::Lit(crate::hir::Lit::Null) => true,
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::FieldAbsent),
            ..
        } => true,
        E::Call {
            callee: Callee::Intrinsic(_),
            args,
        } => matches!(args.as_slice(), [x] if is_absent(x)),
        E::ToDyn { expr, .. } | E::WrapSome(expr) => is_absent(expr),
        _ => false,
    }
}

/// Does type `t` mention an object type of family `base`?
fn mentions_family(cx: &Ctx, t: TyId, base: DefId) -> bool {
    let mut hit = false;
    cx.ty.visit(t, &mut |k| {
        if let TyKind::Adt(d, _) = k {
            hit |= used_family(cx, *d) == Some(base);
        }
    });
    hit
}

/// The fields of `added` the program assigns on a type of family `base` (or through `any`).
fn written_fields(cx: &Ctx, base: DefId, added: &[String]) -> Vec<String> {
    (added.iter())
        .filter(|f| {
            cx.object_copies.writes.iter().any(|w| {
                w.name == **f && w.ty.is_none_or(|t| {
                    let t = cx.ty.opt_payload(t).unwrap_or(t);
                    matches!(cx.ty.kind(t), TyKind::Adt(d, _) if used_family(cx, *d) == Some(base))
                })
            })
        })
        .cloned()
        .collect()
}

/// The name of an extension in family `base` that declares `field`.
fn extension_with(cx: &Ctx, base: DefId, field: &str) -> String {
    let l = &cx.objects.layouts;
    (cx.field_only_of.iter())
        .filter(|(adt, _)| {
            let twin = cx.readonly_twins.get(adt).map(|t| t.0);
            twin.is_some_and(|t| l.redirect.get(&t) == Some(&base))
        })
        .filter(|(adt, _)| {
            cx.adt(**adt)
                .is_some_and(|a| a.fields.iter().any(|f| f.name == field))
        })
        .filter_map(|(_, iface)| cx.iface(*iface).map(|i| (iface.0, i.name.clone())))
        .min()
        .map_or_else(|| "the extension".to_string(), |x| x.1)
}

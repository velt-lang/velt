//! Object values converted by copying their fields (`body::expr::object_copy`): an object type
//! where another object type with the same fields in another order (#651), or with only some
//! of its fields (#650), is expected. TypeScript passes the same object; Velt's object types have
//! fixed layouts, so the converted value is a new object holding the same field values (nested
//! objects are shared, not copied). Once the program is checked, a conversion is an error where
//! the program could tell the copy from the original:
//!
//! - it assigns one of the copied fields, or an optional field the source doesn't have, on a
//!   value of either type (one object would miss it);
//! - it compares values of the converted-to type with `===`, also as a member of a union such
//!   as `A | null` (two copies of one object are not `===`), or uses them as keys of a `Map`,
//!   `Set`, weak map, weak set or weak reference;
//! - it converts a value whose type holds the converted-to type to a dynamic type (`any`,
//!   `unknown`, `object`, `Function`) and compares dynamic values by identity somewhere: `===`
//!   or `==` of them, or a `Map`, `Set`, `WeakMap`, `WeakSet` or `WeakRef` keyed by them;
//! - it prints, serializes (`JSON.stringify`) or lists the keys (`Object.keys`) of a value whose
//!   type holds the converted-to type (Node shows the original's keys, in its order);
//! - it spreads a value of the converted-to type into an object literal (Node copies the
//!   original's fields, all of them);
//! - it tests a value of the converted-to type for a key the copy left out (`"y" in p`).
//!
//! The fix keeps Node's behavior: an object literal with the fields (`{ a: x.a }`) is a new
//! object in TypeScript too. Generic code is checked where it has concrete types (`JSON.stringify` everywhere).

use std::cell::RefCell;

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::copy_flow::{CopyFlow, Store};
use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

/// One conversion that copied the fields `fields` of a `from` value into a new `to`.
pub(crate) struct Copy {
    pub from: TyId,
    pub to: TyId,
    pub fields: Vec<String>,
    /// The optional fields of `to` the source doesn't have: the copy leaves them absent.
    pub absent: Vec<String>,
    pub span: Span,
    /// The source as written (`ab`, `p.owner`), for the fix.
    pub source: Option<String>,
    /// The source is a literal nothing else refers to: only the copy's shape can show.
    pub fresh: bool,
    /// The copy has the source's fields in the source's order, of the same types: of a fresh
    /// source, printing, serializing, listing the keys of or spreading the copy shows what the
    /// original would (an optional field the source lacks is absent in the copy).
    pub in_order: bool,
    /// Made per instantiation of a generic body (`T & { tag }` as `T`, lowered from an
    /// `Upcast`): [`CopyFlow`] does not follow it, so any value converted to a dynamic type
    /// whose type holds `to` may be it.
    pub generic: bool,
}

/// An assignment to field `name` of a value of type `ty` (`None`: a type parameter's, which may
/// be any object type, or one written through `any`; an empty `name`: a key known only at run
/// time, which may name any field).
pub(crate) struct Write {
    pub ty: Option<TyId>,
    pub name: String,
    pub span: Span,
    /// Written through `any` (`x.f = v`, `x[k] = v`, `Reflect.set` with `x: any`): it reaches
    /// only objects the program converts to a dynamic type (`Seen::Dynamic`), or holds in one.
    pub dynamic: bool,
}

/// How a program looks at a value (other than reading its fields).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Seen {
    /// `console.log`.
    Printed,
    /// `JSON.stringify`.
    Serialized,
    /// `Object.keys`.
    Keys,
    /// `===` / `!==`.
    Identity,
    /// `{ ...x }`: the top-level fields.
    Spread,
    /// `"k" in x` for a key `x`'s type lacks: `Copies::in_keys[i]`.
    In(u32),
    /// A value converted to a dynamic type (`ToDyn` of `Program::any_impl`), from where
    /// `Copies::dyn_sources[i]` says.
    Dynamic(u32),
    /// A key compared by identity: of a weak operation, or of a `Map` or `Set` (whose objects
    /// compare by identity, as with `===`).
    Key,
}

/// The conversions, field writes and looks seen so far.
#[derive(Default)]
pub(crate) struct Copies {
    pub copies: Vec<Copy>,
    pub writes: Vec<Write>,
    pub seen: Vec<(TyId, Span, Seen)>,
    /// The keys of `Seen::In` (only ever added to).
    pub in_keys: Vec<String>,
    /// `"k" in x` tests of an optional field without a presence flag: the field, the type of
    /// `x` and the test (`crate::absent_fields`).
    pub in_optional: Vec<(String, TyId, Span)>,
    /// The optional fields without a presence flag an object literal's property gives a value
    /// that may be null, by name (`crate::absent_fields`).
    pub null_literals: Vec<(String, Span)>,
    /// The program's own code constructs a `Proxy` (#823): a conversion that would be reported
    /// but whose source type no object literal builds is `Conversion::ProxyOnly` instead (a
    /// proxy keeps its identity through it, a plain value throws a `TypeError`).
    pub proxies: bool,
    /// Where the value of each `Seen::Dynamic` conversion may come from.
    pub dyn_sources: Vec<Vec<Store>>,
    /// Whether the program's own code has a `new Proxy(...)` (`FnCx::program_makes_proxies`),
    /// once asked.
    pub makes_proxies: Option<bool>,
}

impl Copies {
    /// How many of each there are now (`body::recheck::Lens`).
    pub(crate) fn mark(&self) -> [usize; 5] {
        [
            self.copies.len(),
            self.writes.len(),
            self.seen.len(),
            self.in_optional.len(),
            self.null_literals.len(),
        ]
    }

    /// Drop what was added since `mark`.
    pub(crate) fn rollback(&mut self, mark: [usize; 5]) {
        self.copies.truncate(mark[0]);
        self.writes.truncate(mark[1]);
        self.seen.truncate(mark[2]);
        self.in_optional.truncate(mark[3]);
        self.null_literals.truncate(mark[4]);
    }

    /// The index of key `k` in `in_keys` (for `Seen::In`).
    pub(crate) fn in_key(&mut self, k: &str) -> u32 {
        match self.in_keys.iter().position(|x| x == k) {
            Some(i) => i as u32,
            None => {
                self.in_keys.push(k.to_string());
                (self.in_keys.len() - 1) as u32
            }
        }
    }

    /// The program looks at a value of type `t` at `span`.
    pub(crate) fn observe(&mut self, t: TyId, span: Span, how: Seen) {
        self.seen.push((t, span, how));
    }
}

/// Report the conversions whose copy the program could tell from the original (module docs).
pub(crate) fn check(cx: &mut Ctx) {
    let Copies {
        copies,
        writes,
        seen,
        in_keys,
        proxies,
        dyn_sources,
        ..
    } = std::mem::take(&mut cx.object_copies);
    let spans: Vec<(Span, TyId)> = copies.iter().map(|c| (c.span, c.to)).collect();
    let flow = match seen.iter().any(|s| matches!(s.2, Seen::Dynamic(_))) {
        true => CopyFlow::new(cx, &spans),
        false => CopyFlow::none(),
    };
    let looks = Dynamic {
        seen: &seen,
        sources: &dyn_sources,
        flow: &flow,
        held: Default::default(),
        dynamic: Default::default(),
        generic: Default::default(),
    };
    let built = match proxies {
        true => built_types(cx),
        false => vec![],
    };
    // Conversions allowed only for proxies: a plain source throws instead of being copied.
    let mut strict = vec![];
    for (ci, c) in copies.iter().enumerate() {
        let proxy_only = proxies && !built.iter().any(|&t| related(cx, t, c.from));
        if let Some(w) = assigned(cx, (c, ci), &writes, &looks) {
            match proxy_only {
                true => strict.push((c.span, c.to)),
                false => report_write(cx, c, w),
            }
        } else if let Some(&(t, span, how)) = seen
            .iter()
            .find(|s| sees(cx, (c, ci), s.0, s.2, &in_keys, &looks))
        {
            match proxy_only {
                true => strict.push((c.span, c.to)),
                false => report_seen(cx, c, t, span, how, &in_keys),
            }
        }
    }
    if !strict.is_empty() {
        mark_proxy_only(cx, &strict);
    }
}

/// Mark the conversions at `strict` (span and converted-to type) `Conversion::ProxyOnly`.
fn mark_proxy_only(cx: &mut Ctx, strict: &[(Span, TyId)]) {
    struct Mark<'a>(&'a [(Span, TyId)]);
    impl crate::visit::VisitMut for Mark<'_> {
        fn expr(&mut self, e: &mut crate::hir::Expr) {
            if let crate::hir::ExprKind::AdtLit { converted, .. } = &mut e.kind {
                if *converted == crate::hir::Conversion::Copy && self.0.contains(&(e.span, e.ty)) {
                    *converted = crate::hir::Conversion::ProxyOnly;
                }
            }
        }
    }
    let mut v = Mark(strict);
    for d in cx.defs.iter_mut().flatten() {
        if let crate::hir::Def::Fn(f) = d {
            crate::visit::block(&mut f.body.block, &mut v);
        }
    }
    for i in cx.info.iter_mut() {
        if let crate::defs::DefInfo::Global(g) = i {
            if let Some(e) = &mut g.init {
                crate::visit::expr(e, &mut v);
            }
        }
    }
}

/// An assignment to a copied or absent field of either type of `c` (number `ci`): one through
/// `any` if there is one (the order of the writes depends on the order the modules are checked
/// in).
fn assigned<'w>(
    cx: &mut Ctx,
    (c, ci): (&Copy, usize),
    writes: &'w [Write],
    looks: &Dynamic,
) -> Option<&'w Write> {
    if c.fresh {
        return None;
    }
    // A write through `any` reaches an object only once the program has made it (or a value
    // holding it) an `any`: the copy, or the original, which the copy would not follow.
    let mut reached: Option<bool> = None;
    let mut found = None;
    for w in writes {
        let hit = (w.name.is_empty() || c.fields.contains(&w.name) || c.absent.contains(&w.name))
            && w.ty
                .is_none_or(|t| related(cx, t, c.from) || related(cx, t, c.to))
            && (!w.dynamic || *reached.get_or_insert_with(|| dynamic_reaches(cx, (c, ci), looks)));
        if hit && w.name.is_empty() {
            return Some(w);
        }
        if hit && found.is_none() {
            found = Some(w);
        }
    }
    found
}

/// Does looking at a `t` (`how`) show that `c` made a copy?
fn sees(
    cx: &mut Ctx,
    (c, ci): (&Copy, usize),
    t: TyId,
    how: Seen,
    in_keys: &[String],
    looks: &Dynamic,
) -> bool {
    match how {
        Seen::In(i) => {
            let k = &in_keys[i as usize];
            related(cx, t, c.to)
                && cx
                    .declared_presence(c.from, k)
                    .is_some_and(|p| p != crate::symbols::Presence::No)
        }
        Seen::Identity | Seen::Key => {
            if c.fresh {
                return false;
            }
            let members = compared(cx, t, &mut FxHashSet::default());
            if members.iter().any(|&m| cx.dynamic(m).is_some()) && dynamic_holds(cx, c, ci, looks) {
                return true;
            }
            members.into_iter().any(|m| related(cx, m, c.to))
        }
        Seen::Dynamic(_) => false,
        // A copy of an object type's value (which has exactly its type's fields) with every
        // field in order shows what the original would, as one of a fresh source does.
        Seen::Spread | Seen::Printed | Seen::Serialized | Seen::Keys
            if c.in_order && (c.fresh || anon(cx, c.from)) =>
        {
            false
        }
        Seen::Spread => {
            related(cx, t, c.to) || (cx.dynamic(t).is_some() && dynamic_holds(cx, c, ci, looks))
        }
        _ => {
            looks.holds(cx, t, c.to)
                || (looks.holds_dynamic(cx, t) && dynamic_holds(cx, c, ci, looks))
        }
    }
}

/// The dynamic conversions of a program and where their values come from.
struct Dynamic<'a> {
    seen: &'a [(TyId, Span, Seen)],
    sources: &'a [Vec<Store>],
    flow: &'a CopyFlow,
    /// What the type walks below found, as each copy asks them of each look at a type:
    /// [`holds`] of a type and a copy's type, whether a type can hold a dynamic value, and
    /// [`dynamic_holds`] of a generic copy, which depends only on its type.
    held: RefCell<FxHashMap<(TyId, TyId), bool>>,
    dynamic: RefCell<FxHashMap<TyId, bool>>,
    generic: RefCell<FxHashMap<TyId, bool>>,
}

impl Dynamic<'_> {
    /// [`holds`], remembered.
    fn holds(&self, cx: &mut Ctx, t: TyId, target: TyId) -> bool {
        if let Some(&h) = self.held.borrow().get(&(t, target)) {
            return h;
        }
        let h = holds(cx, t, target, &mut FxHashSet::default());
        self.held.borrow_mut().insert((t, target), h);
        h
    }

    /// Can a value of type `t` hold a dynamic value (`any`, `unknown`, `object`)? Remembered.
    fn holds_dynamic(&self, cx: &mut Ctx, t: TyId) -> bool {
        if let Some(&h) = self.dynamic.borrow().get(&t) {
            return h;
        }
        let is = &mut |cx: &mut Ctx, x| cx.dynamic(x).is_some();
        let h = holds_by(cx, t, is, &mut FxHashSet::default());
        self.dynamic.borrow_mut().insert(t, h);
        h
    }
}

/// Does the program convert a value that may be (or hold) the copy `c` (number `ci`) made to a
/// dynamic type?
fn dynamic_holds(cx: &mut Ctx, c: &Copy, ci: usize, looks: &Dynamic) -> bool {
    if c.generic {
        if let Some(&h) = looks.generic.borrow().get(&c.to) {
            return h;
        }
    }
    let h = looks.seen.iter().any(|&(t, _, how)| match how {
        Seen::Dynamic(i) => {
            (c.generic || looks.flow.carries(ci, &looks.sources[i as usize]))
                && looks.holds(cx, t, c.to)
        }
        _ => false,
    });
    if c.generic {
        looks.generic.borrow_mut().insert(c.to, h);
    }
    h
}

/// Does the program convert the copy `c` (number `ci`), or a value that may hold its source, to
/// a dynamic type? Only then can a write through `any` reach either object.
fn dynamic_reaches(cx: &mut Ctx, (c, ci): (&Copy, usize), looks: &Dynamic) -> bool {
    dynamic_holds(cx, c, ci, looks)
        || looks.seen.iter().any(|&(t, _, how)| {
            matches!(how, Seen::Dynamic(_)) && holds(cx, t, c.from, &mut FxHashSet::default())
        })
}

/// Are `a` and `b` one object type, as conversions see it?
fn related(cx: &mut Ctx, a: TyId, b: TyId) -> bool {
    a == b || cx.canon(a) == cx.canon(b) || cx.same_layout(a, b)
}

/// The object types a `===` operand of type `t` may be: `t` itself, the payload of `T | null`
/// and the members of a union, but not their fields.
fn compared(cx: &mut Ctx, t: TyId, seen: &mut FxHashSet<TyId>) -> Vec<TyId> {
    if !seen.insert(t) {
        return vec![];
    }
    let parts = match cx.ty.opt_payload(t) {
        Some(p) => vec![p],
        None => match cx.union_members(t) {
            Some(members) => members,
            None => return vec![t],
        },
    };
    parts
        .into_iter()
        .flat_map(|p| compared(cx, p, seen))
        .collect()
}

/// Can a value of type `t` hold a `target` (itself, an element, a field, …)?
fn holds(cx: &mut Ctx, t: TyId, target: TyId, seen: &mut FxHashSet<TyId>) -> bool {
    holds_by(cx, t, &mut |cx, x| related(cx, x, target), seen)
}

/// Can a value of type `t` hold a value of a type `is` accepts (itself, an element, a field, …)?
fn holds_by(
    cx: &mut Ctx,
    t: TyId,
    is: &mut dyn FnMut(&mut Ctx, TyId) -> bool,
    seen: &mut FxHashSet<TyId>,
) -> bool {
    if !seen.insert(t) {
        return false;
    }
    if is(cx, t) {
        return true;
    }
    // A brand (a record view, `crate::index_signatures`) holds what its base holds.
    if let Some(base) = cx.brand_base(t) {
        return holds_by(cx, base, is, seen);
    }
    let kind = cx.ty.kind(t).clone();
    let mut parts = crate::types::children(&kind);
    if let Some(members) = cx.union_members(t) {
        parts.extend(members);
    }
    if let TyKind::Adt(d, args) = kind {
        let fields: Vec<TyId> = cx
            .adt(d)
            .map(|a| a.fields.iter().map(|f| f.ty).collect())
            .unwrap_or_default();
        parts.extend(fields.into_iter().map(|f| cx.subst(f, &args)));
    }
    parts.into_iter().any(|p| holds_by(cx, p, is, seen))
}

fn report_write(cx: &mut Ctx, c: &Copy, w: &Write) {
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let (field, label) = if w.name.is_empty() {
        (
            "a field through `any`".to_string(),
            "a field is assigned here".to_string(),
        )
    } else {
        let name = cx.shown_name(&w.name);
        (
            format!("field `{name}`"),
            format!("`{name}` is assigned here"),
        )
    };
    // A copy an interface's getter makes of an object converted to it (`object_assigns.rs`)
    // has no place in the source: the write is the place.
    let d = match c.span == Span::DUMMY {
        true => Diagnostic::error(
            format!(
                "an object converted to an interface holds a `{from}` that the interface reads as a `{to}` by copying its fields, and this assigns {field} of that copy, which the object would not see"
            ),
            w.span,
        ),
        false => Diagnostic::error(
            format!(
                "a value of type `{from}` converts to `{to}` by copying its fields, and this program assigns {field}, which the other object would not see"
            ),
            c.span,
        )
        .with_label(w.span, label),
    };
    let d = with_notes(cx, d, c);
    let d = cx.with_layout_note(d, c.to);
    cx.error(d);
}

fn report_seen(cx: &mut Ctx, c: &Copy, t: TyId, span: Span, how: Seen, in_keys: &[String]) {
    let (from, to, shown) = (cx.display(c.from), cx.display(c.to), cx.display(t));
    // `an` before a vowel sound: `an any`, `an unknown`, `a User`.
    let a = match shown.starts_with(['A', 'E', 'I', 'O', 'a', 'e', 'i', 'o'])
        || shown.starts_with("un")
    {
        true => "an",
        false => "a",
    };
    let what = match how {
        Seen::Printed => format!("prints {a} `{shown}`"),
        Seen::Serialized => format!("serializes {a} `{shown}`"),
        Seen::Keys => format!("lists the keys of {a} `{shown}`"),
        Seen::Identity => format!("compares `{shown}` values with `===` or `!==`"),
        Seen::Key => format!("uses `{shown}` values as keys compared by identity"),
        // Never reported itself (`sees`).
        Seen::Dynamic(_) => format!("converts {a} `{shown}` to a dynamic type"),
        Seen::Spread => format!("spreads {a} `{shown}`"),
        Seen::In(i) => format!(
            "tests {a} `{shown}` for \"{}\" with `in`",
            in_keys[i as usize]
        ),
    };
    let why = match how {
        Seen::Identity => "two copies of one object are not `===`",
        Seen::Key => "two copies of one object are different keys",
        Seen::Spread => "Node would copy all the original object's fields, in its order",
        Seen::In(_) => "the copy lacks the original's other fields, which Node would find",
        _ => "Node would show the original object's fields, in its order",
    };
    let d = Diagnostic::error(
        format!("a value of type `{from}` converts to `{to}` by copying its fields, and this program {what}: {why}"),
        // A copy an interface's getter makes (`object_assigns.rs`) has no place of its own.
        if c.span == Span::DUMMY { span } else { c.span },
    )
    .with_label(span, "here, or in the function this calls");
    let d = with_notes(cx, d, c);
    let d = cx.with_layout_note(d, c.to);
    cx.error(d);
}

fn with_notes(cx: &mut Ctx, d: Diagnostic, c: &Copy) -> Diagnostic {
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let src = c.source.as_deref().unwrap_or("x");
    // A symbol key as written: `[KEY]: x[KEY]`.
    let fields: Vec<String> = (c.fields.iter())
        .map(|f| match cx.key_spelling(f) {
            Some(k) => format!("{k}: {src}{k}"),
            None => format!("{f}: {src}.{f}"),
        })
        .collect();
    let fields = match c.source {
        Some(_) => format!("`{{ {} }}`", fields.join(", ")),
        // An element of a converted array, or another value without a name.
        None => format!("`(x) => ({{ {} }})` with `.map`", fields.join(", ")),
    };
    d.with_note(format!(
        "TypeScript allows this and passes the same object; Velt's object types have fixed layouts, so the `{to}` made from a `{from}` value is a new object with the same field values"
    ))
    .with_note(format!(
        "build the new object explicitly, {fields} (a new object in TypeScript too), or declare the parameter or variable as `{from}`"
    ))
}

/// The types of the object literals (`AdtLit`) in the program's bodies and constants.
fn built_types(cx: &mut Ctx) -> Vec<TyId> {
    struct Lits(FxHashSet<TyId>);
    impl crate::visit::VisitMut for Lits {
        fn expr(&mut self, e: &mut crate::hir::Expr) {
            if let crate::hir::ExprKind::AdtLit { .. } = e.kind {
                self.0.insert(e.ty);
            }
        }
    }
    let mut v = Lits(FxHashSet::default());
    for d in cx.defs.iter_mut().flatten() {
        if let crate::hir::Def::Fn(f) = d {
            crate::visit::block(&mut f.body.block, &mut v);
        }
    }
    for i in cx.info.iter_mut() {
        if let crate::defs::DefInfo::Global(g) = i {
            if let Some(e) = &mut g.init {
                crate::visit::expr(e, &mut v);
            }
        }
    }
    v.0.into_iter().collect()
}

/// Is `t` an anonymous object type?
fn anon(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Adt(d, _) if cx.adt(*d).is_some_and(|a| a.kind == crate::hir::AdtKind::Anon))
}

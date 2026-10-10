//! What the program's function bodies do with fields (`Bodies`).

use super::*;

/// What the bodies of the program's functions do with fields, read once.
#[derive(Default)]
pub(super) struct Bodies {
    /// The fields each closure literal is assigned to (`i.onChange = (v) => …`,
    /// `{ f: () => … }`), as `(class or object type, field)`.
    pub(super) stores: HashMap<DefId, Vec<(DefId, u32)>>,
    /// Per function: the names of the fields it reads, or `None` when it reads one whose name
    /// is not known. By name, so a copy of an object read through another type counts too;
    /// assigning a field is not reading it.
    reads: HashMap<DefId, Option<HashSet<String>>>,
    /// The names of the fields any code of the program reads other than to call what it reads
    /// right there (`const h = w.onClick` copies the closure out, where a request may get it;
    /// `w.onClick()` in `main` runs it in `main`), field initializers and module constants
    /// included; `None`: one whose name is not known.
    escapes: Option<HashSet<String>>,
    /// Per function: the functions of the program it calls directly and the closures it makes
    /// (`xs.forEach((x) => x.onChange(""))` runs the closure; counted as a call).
    calls: HashMap<DefId, Vec<DefId>>,
    /// The program's methods and the functions it uses as values (`const f = poke`): any of
    /// them may run in a request.
    entries: Vec<DefId>,
    /// Per closure: the nodes of the locals it is stored into ([`super::held::homes`]).
    pub(super) homes: HashMap<DefId, Vec<usize>>,
}

impl Bodies {
    pub(super) fn new(cx: &Ctx) -> Bodies {
        let mut out = Bodies {
            escapes: Some(HashSet::new()),
            ..Bodies::default()
        };
        for (i, def) in cx.defs.iter().enumerate() {
            let d = DefId(i as u32);
            let mut reads = Some(HashSet::new());
            let mut uses = FieldUses {
                cx,
                reads: &mut reads,
                escapes: &mut out.escapes,
                next: None,
            };
            let f = match def {
                // Field initializers and module constants run code too.
                Some(Def::Adt(a)) => {
                    for x in a.fields.iter().filter_map(|f| f.default.as_ref()) {
                        walk::expr(x, &mut uses);
                    }
                    out.entries.extend(a.ctor);
                    continue;
                }
                Some(Def::Global(gl)) => {
                    walk::expr(&gl.init, &mut uses);
                    continue;
                }
                Some(Def::Fn(f)) if !in_std(cx, d) => f,
                _ => continue,
            };
            walk::block(&f.body.block, &mut uses);
            if f.self_ty.is_some() {
                out.entries.push(d);
            }
            let mut calls = vec![];
            each_expr(&f.body.block, &mut |e: &Expr| match &e.kind {
                E::Assign { place, value } => {
                    if let (E::Field { base, index, .. }, E::Closure(c)) =
                        (&place.kind, &value.kind)
                    {
                        if let TyKind::Adt(a, _) = cx.ty.kind(base.ty) {
                            out.stores.entry(*c).or_default().push((*a, *index));
                        }
                    }
                }
                E::AdtLit { def, fields, .. } => {
                    for (k, x) in fields.iter().enumerate() {
                        if let E::Closure(c) = x.kind {
                            out.stores.entry(c).or_default().push((*def, k as u32));
                        }
                    }
                }
                E::Call {
                    callee: Callee::Def(g, _),
                    ..
                } if !in_std(cx, *g) => calls.push(*g),
                E::Closure(k) => calls.push(*k),
                E::FnRef(g, _) if !in_std(cx, *g) => out.entries.push(*g),
                _ => {}
            });
            out.reads.insert(d, reads);
            out.calls.insert(d, calls);
        }
        out
    }

    /// The names of the fields that may be read where a request gets what they hold: by the
    /// closures `reached`, the program's methods and functions used as values, what they call
    /// and the closures they make (all of them may run in a request), and by any code that
    /// copies a field's value out ([`Bodies::escapes`]). `None`: unknown.
    pub(super) fn request_reads(
        &self,
        reached: impl Iterator<Item = DefId>,
    ) -> Option<HashSet<String>> {
        let mut out = self.escapes.clone()?;
        let mut work: Vec<DefId> = reached.chain(self.entries.iter().copied()).collect();
        let mut seen = HashSet::new();
        while let Some(d) = work.pop() {
            if !seen.insert(d) {
                continue;
            }
            match self.reads.get(&d) {
                Some(Some(names)) => out.extend(names.iter().cloned()),
                Some(None) => return None,
                None => {}
            }
            work.extend(self.calls.get(&d).into_iter().flatten().copied());
        }
        Some(out)
    }
}

/// Collects the names of the fields a body uses, in expressions and patterns.
struct FieldUses<'a, 'm> {
    cx: &'a Ctx<'m>,
    /// What the body reads.
    reads: &'a mut Option<HashSet<String>>,
    /// What the program reads other than to call it right away ([`Bodies::escapes`]).
    escapes: &'a mut Option<HashSet<String>>,
    /// The expression visited next when it is the place of an assignment (`x.f = …` writes
    /// `f`, `false`) or the callee of a call (`x.f()` calls what `f` holds, `true`).
    next: Option<(*const Expr, bool)>,
}

impl FieldUses<'_, '_> {
    /// A use of field `index` of a value of type `ty`; `escapes`: other than called right away.
    fn field(&mut self, ty: TyId, index: u32, escapes: bool) {
        let name = match self.cx.ty.kind(ty) {
            TyKind::Adt(a, _) => self
                .cx
                .adt(*a)
                .and_then(|i| i.fields.get(index as usize))
                .map(|f| f.name.clone()),
            _ => None,
        };
        let mut sets = vec![&mut *self.reads];
        if escapes {
            sets.push(&mut *self.escapes);
        }
        for set in sets {
            match (&name, set.as_mut()) {
                (Some(n), Some(names)) => {
                    names.insert(n.clone());
                }
                _ => *set = None,
            }
        }
    }

    fn pat(&mut self, p: &Pat) {
        match &p.kind {
            PatKind::Adt { fields } => {
                for (k, sub) in fields {
                    self.field(p.ty, *k, true);
                    self.pat(sub);
                }
            }
            PatKind::Variant { args: xs, .. }
            | PatKind::Tuple(xs)
            | PatKind::Or(xs)
            | PatKind::Array { elems: xs, .. } => xs.iter().for_each(|x| self.pat(x)),
            PatKind::Some(x) => self.pat(x),
            _ => {}
        }
    }
}

impl Visit for FieldUses<'_, '_> {
    fn stmt(&mut self, s: &Stmt) {
        if let S::LetPat { pat, .. } | S::ForOf { binding: pat, .. } = &s.kind {
            self.pat(pat);
        }
    }

    fn expr(&mut self, e: &Expr) {
        let next = self.next.take().filter(|(x, _)| std::ptr::eq(*x, e));
        match &e.kind {
            // Visited next: the place, the callee.
            E::Assign { place, .. } => self.next = Some((&**place, false)),
            E::Call {
                callee: Callee::Indirect(c),
                ..
            } => self.next = Some((&**c, true)),
            E::Field { base, index, .. } => match next {
                Some((_, false)) => {}
                Some((_, true)) => self.field(base.ty, *index, false),
                None => self.field(base.ty, *index, true),
            },
            E::Match { arms, .. } => arms.iter().for_each(|a| self.pat(&a.pat)),
            _ => {}
        }
    }
}

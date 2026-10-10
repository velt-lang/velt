//! What the program's function bodies do with fields (`Bodies`).

use super::*;

/// What the bodies of the program's functions do with fields, read once.
#[derive(Default)]
pub(super) struct Bodies {
    /// The fields each closure literal is assigned to (`i.onChange = (v) => …`,
    /// `{ f: () => … }`), as `(class or object type, field)`.
    pub(super) stores: HashMap<DefId, Vec<(DefId, u32)>>,
    /// Per function: the names of the fields it reads (or writes), or `None` when it uses one
    /// whose name is not known.
    reads: HashMap<DefId, Option<HashSet<String>>>,
    /// Per function: the functions of the program it calls directly.
    calls: HashMap<DefId, Vec<DefId>>,
    /// The program's methods and the functions it uses as values (`const f = poke`): any of
    /// them may run in a request.
    entries: Vec<DefId>,
    /// Per closure: the nodes of the locals it is stored into ([`held_closures`]).
    pub(super) homes: HashMap<DefId, Vec<usize>>,
}

impl Bodies {
    pub(super) fn new(cx: &Ctx) -> Bodies {
        let mut out = Bodies::default();
        for (i, def) in cx.defs.iter().enumerate() {
            let d = DefId(i as u32);
            let Some(Def::Fn(f)) = def else { continue };
            if in_std(cx, d) {
                continue;
            }
            if f.self_ty.is_some() {
                out.entries.push(d);
            }
            let mut reads = Some(HashSet::new());
            walk::block(
                &f.body.block,
                &mut FieldUses {
                    cx,
                    reads: &mut reads,
                },
            );
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
                E::FnRef(g, _) if !in_std(cx, *g) => out.entries.push(*g),
                _ => {}
            });
            out.reads.insert(d, reads);
            out.calls.insert(d, calls);
        }
        out
    }

    /// The names of the fields that code a request may run uses: the closures `reached`, the
    /// program's methods and functions used as values, and what they call. `None`: unknown.
    pub(super) fn request_reads(
        &self,
        reached: impl Iterator<Item = DefId>,
    ) -> Option<HashSet<String>> {
        let mut work: Vec<DefId> = reached.chain(self.entries.iter().copied()).collect();
        let mut seen = HashSet::new();
        let mut out = HashSet::new();
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
    reads: &'a mut Option<HashSet<String>>,
}

impl FieldUses<'_, '_> {
    fn field(&mut self, ty: TyId, index: u32) {
        let name = match self.cx.ty.kind(ty) {
            TyKind::Adt(a, _) => self
                .cx
                .adt(*a)
                .and_then(|i| i.fields.get(index as usize))
                .map(|f| f.name.clone()),
            _ => None,
        };
        match (name, self.reads.as_mut()) {
            (Some(n), Some(reads)) => {
                reads.insert(n);
            }
            _ => *self.reads = None,
        }
    }

    fn pat(&mut self, p: &Pat) {
        match &p.kind {
            PatKind::Adt { fields } => {
                for (k, sub) in fields {
                    self.field(p.ty, *k);
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
        if let S::LetPat { pat, .. } = &s.kind {
            self.pat(pat);
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Field { base, index, .. } => self.field(base.ty, *index),
            E::Match { arms, .. } => arms.iter().for_each(|a| self.pat(&a.pat)),
            _ => {}
        }
    }
}

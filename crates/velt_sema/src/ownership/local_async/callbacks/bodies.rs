//! Where the program's function bodies store closures (`Bodies`), for the message only: which
//! closures a handler reaches does not depend on it.

use super::*;

/// Where the bodies of the program's functions store closures, read once.
#[derive(Default)]
pub(super) struct Bodies {
    /// The fields each closure literal is assigned to (`i.onChange = (v) => …`,
    /// `{ f: () => … }`), as `(class or object type, field)`: a call of such a field is the
    /// call shown first.
    pub(super) stores: HashMap<DefId, Vec<(DefId, u32)>>,
    /// Per closure: the nodes of the locals it is stored into ([`super::held::homes`]).
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
                _ => {}
            });
        }
        out
    }
}

//! Binding the instances the driver generated ([`super::GenInput`]) to their definitions: the
//! declared generated function and its type arguments (type aliases the generated code
//! declares) identify the instance, the generated function item is its body.

use rustc_hash::FxHashMap;
use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::hir::{DefId, TyId};
use crate::resolve::TyEnv;

/// `(generated function, concrete type arguments)` → the generated definition of its body.
pub(crate) type Bound = FxHashMap<(DefId, Vec<TyId>), DefId>;

pub(crate) fn bind(cx: &mut Ctx) -> Bound {
    let mut out = Bound::default();
    let Some(gen) = cx.gen else {
        return out;
    };
    for inst in &gen.instances {
        let Some(m) = cx.modules.iter().position(|s| s.path == inst.module) else {
            ice_missing(cx, &inst.module, "module");
            continue;
        };
        let def = |cx: &Ctx, name: &str| match cx.scopes[m].items.get(name) {
            Some(Item::Def(d)) => Some(*d),
            _ => None,
        };
        let (Some(f), Some(body)) = (def(cx, &inst.function), def(cx, &inst.body)) else {
            ice_missing(cx, &inst.function, "generated function");
            continue;
        };
        let env = TyEnv::new(m, &[]);
        let targs: Vec<TyId> = (inst.type_args.iter())
            .map(|name| {
                let t = ast::TypeExpr {
                    kind: ast::TypeExprKind::Named {
                        path: vec![ast::Ident {
                            name: name.clone(),
                            span: Span::DUMMY,
                        }],
                        args: vec![],
                    },
                    span: Span::DUMMY,
                };
                cx.resolve_type(&t, &env)
            })
            .collect();
        out.insert((f, targs), body);
    }
    out
}

fn ice_missing(cx: &mut Ctx, name: &str, what: &str) {
    cx.error(Diagnostic::error(
        format!(
            "internal compiler error: generated code names a {what} `{name}` that is not there"
        ),
        Span::DUMMY,
    ));
}

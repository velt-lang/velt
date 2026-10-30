//! Conversions from `any` to a function type that lowering makes inside another conversion
//! (#1935): a function held by a record's or an object's member converted to an object type
//! (`slots.default`, stored as `(scopedProps?: any) => any[]` and read as `() => VNode[]`), or
//! an element of an array of functions. Lowering converts such members itself (velt_vir
//! `any_record.rs`, `any_object_fill.rs`, array views), where the value is a function of
//! another type than the target. TypeScript accepts it (a function with optional parameters is
//! one with fewer, `any[]` is any array), and Node calls the function as it is.
//!
//! So sema makes, once per such function type `T`, the function `(f: any) => T` returning the
//! closure a direct conversion makes (`FnCx::dynamic_as_fn`): it calls `f` through `any`, with
//! the arguments it is given (extra ones dropped, missing ones `undefined`), and converts the
//! result to `T`'s. Lowering calls it when the value is a function of another type
//! (`hir::Program::any_fn_views`); the closure is made at the conversion, so a call of the
//! converted function costs one call through `any`.

use std::collections::hash_map::Entry;

use velt_common::Span;

use crate::body::{FnCx, Frame, LocalKind};
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, ParamSig};
use crate::hir::{self, AdtKind, Def, DefId, ExprKind as H, PassMode, TyId, TyKind, UseMode};
use crate::resolve::TyEnv;

/// The function types lowering may convert a dynamic value to inside another conversion, and
/// their conversion functions (module docs).
#[derive(Default)]
pub(crate) struct FnViews {
    /// Each function type seen, with its conversion function once made (`None`: it has none,
    /// lowering keeps the exact check).
    pub made: rustc_hash::FxHashMap<TyId, Option<DefId>>,
    /// Types seen and not made yet, with the module and the span of the conversion.
    pub pending: Vec<(TyId, usize, Span)>,
    /// A target written with type parameters holds a function type (`o as { list: () => T[] }`
    /// in `read<T>`): the module of the first one. Its instantiations are found once every body
    /// is checked ([`make_fn_views`]).
    pub generic: Option<usize>,
}

impl FnViews {
    /// The conversion functions made, in order (`hir::Program::any_fn_views`).
    pub(crate) fn into_program(self) -> Vec<DefId> {
        let mut out: Vec<DefId> = self.made.into_values().flatten().collect();
        out.sort_unstable_by_key(|d| d.0);
        out
    }
}

/// How deep the member types of a conversion's target are looked through.
const DEPTH: u32 = 8;

impl FnCx<'_, '_> {
    /// Note the function types among the members of `exp`, the target of a conversion of a
    /// dynamic value (module docs): the fields of an object type, the elements of an array or
    /// tuple, the members of a union or option. `exp` itself is converted here
    /// (`dynamic_as_fn`).
    pub(super) fn note_fn_views(&mut self, exp: TyId, span: Span) {
        self.cx.note_fn_views_in(exp, self.module, span);
    }
}

impl Ctx<'_> {
    fn note_fn_views_in(&mut self, exp: TyId, module: usize, span: Span) {
        for t in self.fn_members(exp) {
            if self.has_type_params(t) {
                self.fn_views.generic.get_or_insert(module);
            } else if let Entry::Vacant(e) = self.fn_views.made.entry(t) {
                e.insert(None);
                self.fn_views.pending.push((t, module, span));
            }
        }
    }

    /// The function types among the members of `t` (`note_fn_views`), not `t` itself.
    fn fn_members(&mut self, t: TyId) -> Vec<TyId> {
        let (mut out, mut seen) = (vec![], vec![]);
        let mut work = vec![(t, 0)];
        while let Some((t, depth)) = work.pop() {
            if depth > DEPTH || seen.contains(&t) {
                continue;
            }
            seen.push(t);
            let inner: Vec<TyId> = match self.ty.kind(t).clone() {
                TyKind::FnPtr { .. } => {
                    if depth > 0 {
                        out.push(t);
                    }
                    continue;
                }
                TyKind::Array(x) | TyKind::Option(x, _) => vec![x],
                TyKind::Tuple(xs) => xs,
                TyKind::Adt(d, args) => match &self.info[d.0 as usize] {
                    DefInfo::Adt(a) if a.kind != AdtKind::Class => {
                        let fields: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
                        fields.iter().map(|f| self.subst(*f, &args)).collect()
                    }
                    _ => self.union_members(t).unwrap_or_default(),
                },
                _ => vec![],
            };
            work.extend(inner.into_iter().map(|x| (x, depth + 1)));
        }
        out
    }
}

/// The conversion functions (module docs), then the adapters to generic interfaces and the
/// object types' interface impls of the conversions recorded so far, those in the conversion
/// functions' bodies included (an interface result, `get: () => Item`), then the conversion
/// functions those two noted.
pub(crate) fn make_dynamic_impls(cx: &mut Ctx) {
    make_fn_views(cx);
    super::iface_adapt_dynamic::make_dynamic_iface_adapters(cx);
    crate::object_ifaces::make_dynamic_object_impls(cx);
    make_fn_views(cx);
}

/// Make the conversion function of every function type noted (module docs), and of those the
/// instantiations of generic targets give. Making one may note more (a result converted to an
/// object type holding functions).
fn make_fn_views(cx: &mut Ctx) {
    if let Some(module) = cx.fn_views.generic.take() {
        let mut keep = |cx: &mut Ctx, t: TyId| {
            let fns = cx.fn_members(t);
            fns.into_iter().any(|f| cx.has_type_params(f))
        };
        for t in crate::any_iface_sites::instantiated_targets(cx, &mut keep) {
            cx.note_fn_views_in(t, module, Span::DUMMY);
        }
    }
    while let Some((t, module, span)) = cx.fn_views.pending.pop() {
        let made = make_view(cx, t, module, span);
        cx.fn_views.made.insert(t, made);
    }
}

/// The function `(f: any) => T` for the function type `t`: the closure calling `f` through
/// `any`. `None` when there is no `any` or `t` has no such conversion (a result of a promise
/// type, which no `any` holds).
fn make_view(cx: &mut Ctx, t: TyId, module: usize, span: Span) -> Option<DefId> {
    let defs = cx.any?;
    let any = cx.ty.intern(TyKind::Dyn(defs.any, vec![]));
    let TyKind::FnPtr { params, .. } = cx.ty.kind(t).clone() else {
        return None;
    };
    let name = format!("<any as {}>", cx.display(t));
    let mut info =
        crate::collect::fn_placeholder(name.clone(), span, span, module, FnKind::Free, None);
    info.params = vec![ParamSig {
        name: "f".to_string(),
        span,
        ty: any,
        mode: PassMode::Owned,
        default: None,
    }];
    info.ret = t;
    info.fixed_modes = true;
    info.state = BodyState::Done;
    let def = cx.alloc_def(span, DefInfo::Fn(Box::new(info)));
    let env = TyEnv::new(module, &[]);
    let mut g = FnCx::new(cx, module, env, Frame::new(FnKind::Free, Some(t)));
    g.fn_name = name.clone();
    g.body_def = Some(def);
    g.cx.checking.push(def);
    let f = g.new_local("f", any, false, span, LocalKind::Param);
    let held = g.mk(H::Local(f, UseMode::Move), any, span);
    let (value, made) = match g.dynamic_as_fn(held, t, params.len()) {
        Ok(v) => (v, true),
        Err(_) => {
            // Never called (lowering keeps the exact check): a body all the same.
            let msg = g.str_lit(
                "ICE: an `any` converted to a function type it has none to",
                span,
            );
            let never = g.cx.ty.never;
            (
                g.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span),
                false,
            )
        }
    };
    g.cx.checking.pop();
    let frame = std::mem::replace(&mut g.f, Frame::new(FnKind::Free, None));
    drop(g);
    let fndef = hir::FnDef {
        name,
        generics: 0,
        params: vec![hir::Param {
            local: f,
            ty: any,
            mode: PassMode::Owned,
        }],
        ret: t,
        is_async: false,
        is_generator: false,
        self_ty: None,
        captures: vec![],
        shares_captures: false,
        body: hir::Body {
            locals: frame.locals,
            block: hir::Block {
                stmts: vec![hir::Stmt {
                    kind: hir::StmtKind::Return(Some(value)),
                    span,
                }],
                value: None,
                span,
            },
        },
        throws: None,
        span,
    };
    cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
    let info = cx.fn_info_mut(def);
    info.local_kinds = frame.kinds;
    info.throw_srcs = frame.uncaught;
    made.then_some(def)
}

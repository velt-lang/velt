//! `any` and `unknown`, the dynamic value (issue #924, design note on #823; hir_encodings.md
//! "any and unknown").
//!
//! Both are interfaces of the prelude (`std/prelude/z_dynamic.vlt`) that every type implements
//! through one generic implementation sema adds here (`Program::any_impl`, `impl<T> any for
//! T` with no methods). A value converted to either is an interface value (`ToDyn`): the
//! value, boxed unless it is a class instance or a counted object, and the interface table
//! of its concrete type, which is its type descriptor. Both types share it, so converting
//! between them changes nothing at run time. `T | null` and unions are unpacked when they are
//! boxed: a dynamic value always holds one concrete type, and `null` / `undefined` hold the
//! nothing of `boolean | null` / `boolean | undefined`.
//!
//! `object` and `Function` (issue #926) are two more interfaces of the prelude with the same
//! representation: the dynamic value restricted to non-primitive values and to functions. Only
//! such values convert to them (`Ctx::is_object_ty`, `is_function_ty`), so `T extends object`
//! tells objects from primitives; they convert to `any` / `unknown` as they are.

use crate::ctx::Ctx;
use rustc_hash::FxHashMap;

use crate::defs::{is_private_key, is_setter_key, key_member_name};
use crate::hir::{DefId, DynMember, DynMemberKind, ImplDef, Nothing, TyId, TyKind};

/// The prelude's dynamic interfaces and their implementation (`Ctx::any`): `any` / `unknown`
/// and the implementation every type has.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AnyDefs {
    pub any: DefId,
    pub unknown: DefId,
    /// TypeScript's `object`: a dynamic value that holds no primitive.
    pub object: DefId,
    /// TypeScript's `Function`: a dynamic value that holds a function.
    pub function: DefId,
    /// Index of `impl<T> any for T` in `Ctx::impls`, added when a value is first converted
    /// to `any` or `unknown` ([`Ctx::any_impl`]): a program without them is unchanged.
    pub imp: Option<u32>,
    /// A member of an `any` is read or called somewhere (`Program::dyn_members`).
    pub members: bool,
}

/// Which dynamic type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dynamic {
    Any,
    Unknown,
    /// `object`: members need narrowing first, as on an `unknown`.
    Object,
    /// `Function`: calls go through the descriptor, as on an `any`.
    Function,
}

impl Dynamic {
    /// The name of the type, as written.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Dynamic::Any => "any",
            Dynamic::Unknown => "unknown",
            Dynamic::Object => "object",
            Dynamic::Function => "Function",
        }
    }

    /// The article before the name (`an any`, `a Function`).
    pub(crate) fn article(self) -> &'static str {
        match self {
            Dynamic::Function => "a",
            _ => "an",
        }
    }
}

/// After the declarations: find the prelude's dynamic interfaces.
pub(crate) fn add_impl(cx: &mut Ctx) {
    let (Some(any), Some(unknown)) = (cx.prelude_iface("any"), cx.prelude_iface("unknown")) else {
        return;
    };
    let (Some(object), Some(function)) = (cx.prelude_iface("object"), cx.prelude_iface("Function"))
    else {
        return;
    };
    cx.any = Some(AnyDefs {
        any,
        unknown,
        object,
        function,
        imp: None,
        members: false,
    });
}

impl Ctx<'_> {
    /// The index of `impl<T> any for T`, added on first use.
    pub(crate) fn any_impl(&mut self) -> Option<u32> {
        let defs = self.any?;
        if let Some(i) = defs.imp {
            return Some(i);
        }
        let t = self.ty.intern(TyKind::Param(0));
        self.impls.push(ImplDef {
            ty: t,
            generics: 1,
            iface: defs.any,
            iface_args: vec![],
            methods: vec![],
        });
        let i = self.impls.len() as u32 - 1;
        self.any = Some(AnyDefs {
            imp: Some(i),
            ..defs
        });
        Some(i)
    }

    /// Are `a` and `b` one type but for `any` in one where the other has `unknown`, inside
    /// arrays, tuples, nullables, `Map`s, `Record`s, promises and function types
    /// (`Record<string, any>` and `Record<string, unknown>`)? The two hold the same value, laid
    /// out the same way, and each takes every value the other does, so a container of one is
    /// the container of the other, read and written in place. A type with other parts (a
    /// class or an interface at such arguments) is not: its members may depend on the argument.
    pub(crate) fn same_but_any_unknown(&self, a: TyId, b: TyId) -> bool {
        a != b && self.any_unknown_equal(a, b)
    }

    fn any_unknown_equal(&self, a: TyId, b: TyId) -> bool {
        if a == b {
            return true;
        }
        let top = |t: TyId| matches!(self.dynamic(t), Some(Dynamic::Any) | Some(Dynamic::Unknown));
        if top(a) && top(b) {
            return true;
        }
        let all = |xs: &[TyId], ys: &[TyId]| {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(x, y)| self.any_unknown_equal(*x, *y))
        };
        match (self.ty.kind(a), self.ty.kind(b)) {
            (TyKind::Array(x), TyKind::Array(y)) => self.any_unknown_equal(*x, *y),
            (TyKind::Option(x, n), TyKind::Option(y, m)) => {
                n == m && self.any_unknown_equal(*x, *y)
            }
            (TyKind::Map(k, v), TyKind::Map(l, w))
            | (TyKind::Promise(k, v), TyKind::Promise(l, w)) => {
                self.any_unknown_equal(*k, *l) && self.any_unknown_equal(*v, *w)
            }
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) => all(xs, ys),
            (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) => {
                d == e
                    && ["Record", "Map", "Set"]
                        .iter()
                        .any(|n| self.prelude_adt(n) == Some(*d))
                    && all(xs, ys)
            }
            (
                TyKind::FnPtr {
                    params: p,
                    ret: r,
                    throws: t,
                    rest: x,
                },
                TyKind::FnPtr {
                    params: q,
                    ret: s,
                    throws: u,
                    rest: y,
                },
            ) => x == y && t == u && all(p, q) && self.any_unknown_equal(*r, *s),
            _ => false,
        }
    }

    /// Is `t` `any` or `unknown`?
    pub(crate) fn dynamic(&self, t: TyId) -> Option<Dynamic> {
        let defs = self.any?;
        match self.ty.kind(t) {
            TyKind::Dyn(d, _) if *d == defs.any => Some(Dynamic::Any),
            TyKind::Dyn(d, _) if *d == defs.unknown => Some(Dynamic::Unknown),
            TyKind::Dyn(d, _) if *d == defs.object => Some(Dynamic::Object),
            TyKind::Dyn(d, _) if *d == defs.function => Some(Dynamic::Function),
            _ => None,
        }
    }

    /// Is `t` a type whose values are no primitives (TypeScript's `object`): class and
    /// struct instances, object types, arrays, tuples, maps, functions, interface values and
    /// the dynamic types other than `unknown`? A union is one when each member is; enums,
    /// brands, literal types and `T | null` are not. A type parameter is one only through its
    /// bound (`Ctx::satisfies`).
    pub(crate) fn is_object_ty(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Array(_)
            | TyKind::Map(..)
            | TyKind::Tuple(_)
            | TyKind::Promise(..)
            | TyKind::Shared(_)
            | TyKind::Result(..)
            | TyKind::FnPtr { .. }
            | TyKind::Closure(_) => true,
            TyKind::Dyn(..) => self.dynamic(t) != Some(Dynamic::Unknown),
            // A deferred intersection, through its parts (`Ctx::holds_objects`).
            TyKind::Op(crate::hir::TyOp::Intersect, _) => self.holds_objects(t, &[]),
            TyKind::Adt(d, _) => {
                if let Some((_, members)) = self.union_def(t) {
                    return members.iter().all(|m| self.is_object_ty(*m));
                }
                self.enum_info(*d).is_none()
                    && self.brand_base(t).is_none_or(|b| self.is_object_ty(b))
            }
            _ => false,
        }
    }

    /// Is `t` a function type (what TypeScript's `Function` takes), or `Function` / `any`?
    pub(crate) fn is_function_ty(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::FnPtr { .. } | TyKind::Closure(_) => true,
            // A function with properties (#921).
            TyKind::Adt(..) => self.is_callable_object(t),
            TyKind::Dyn(..) => matches!(
                self.dynamic(t),
                Some(Dynamic::Function) | Some(Dynamic::Any)
            ),
            _ => false,
        }
    }

    /// Does the dynamic interface `iface` take values of type `t`? Every type is an `any` and
    /// an `unknown`; `object` takes non-primitive types and `Function` function types.
    /// `None` when `iface` is no dynamic interface.
    pub(crate) fn dynamic_takes(&self, iface: DefId, t: TyId) -> Option<bool> {
        let defs = self.any?;
        if iface == defs.any || iface == defs.unknown {
            Some(true)
        } else if iface == defs.object {
            Some(self.is_object_ty(t))
        } else if iface == defs.function {
            Some(self.is_function_ty(t))
        } else {
            None
        }
    }

    /// Is `t` `any` or `unknown`, or one of them or `null` (`any | null`)?
    pub(crate) fn maybe_dynamic(&self, t: TyId) -> bool {
        let t = self.ty.core(t).unwrap_or(t);
        self.dynamic(t).is_some()
    }

    /// The dynamic `undefined` as an `exp` (`any` / `unknown`): what a left-out `q?: any`
    /// field holds.
    pub(crate) fn dynamic_undefined(
        &mut self,
        exp: TyId,
        span: velt_common::Span,
    ) -> Option<crate::hir::Expr> {
        use crate::hir::{Expr, ExprKind, Lit};
        let impl_index = self.any_impl()?;
        let ty = self.dynamic_nothing_ty(Nothing::Undefined);
        let null = Expr {
            kind: ExprKind::Lit(Lit::Null),
            ty,
            span,
        };
        let kind = ExprKind::ToDyn {
            expr: Box::new(null),
            impl_index,
        };
        Some(Expr {
            kind,
            ty: exp,
            span,
        })
    }

    /// The type of the value a dynamic `null` (`Nothing::Null`) or `undefined` holds:
    /// `boolean | null` / `boolean | undefined`, whose nothing it is.
    pub(crate) fn dynamic_nothing_ty(&mut self, n: Nothing) -> TyId {
        let b = self.ty.bool_;
        let n = if n.has_null() {
            Nothing::Null
        } else {
            Nothing::Undefined
        };
        self.ty.intern(TyKind::Option(b, n))
    }
}

/// `Program::dyn_members`: every struct / class / object type's methods and accessors by name
/// (own ones first, then each base class's that are not overridden), then its `readonly`
/// fields, when a member of an `any` is read, called or written; `None` otherwise.
pub(crate) fn dyn_members(cx: &mut Ctx) -> Option<FxHashMap<DefId, Vec<DynMember>>> {
    if !cx.any.is_some_and(|a| a.members) && !accessor_literals_dynamic(cx) {
        return None;
    }
    let mut out = FxHashMap::default();
    for i in 0..cx.info.len() {
        let d = DefId(i as u32);
        let Some(a) = cx.adt(d) else { continue };
        let generics = a.generics.names.len() as u32;
        let args: Vec<TyId> = (0..generics)
            .map(|k| cx.ty.intern(TyKind::Param(k)))
            .collect();
        let mut members: Vec<DynMember> = vec![];
        let (mut cur, mut cur_args) = (d, args);
        for _ in 0..64 {
            let Some(a) = cx.adt(cur) else { break };
            // A standard-library class's `private` members are not in its `any` table: no
            // program can name them (Node's `RegExp` has no `matchAt`), and their glue would only
            // cost code, and make the types it converts arguments to view types (#1831). A
            // program's own `private` members stay, as in Node.
            let std = cx.scopes[a.module].is_std;
            let mut own: Vec<(String, DefId)> = a
                .methods
                .iter()
                .filter(|(k, m)| !m.is_static && !is_private_key(k))
                .filter(|(_, m)| !(std && cx.fn_info(m.def).is_private))
                .map(|(k, m)| (k.clone(), m.def))
                .collect();
            own.sort();
            let base = a.base;
            let owner = cx.ty.intern(TyKind::Adt(cur, cur_args.clone()));
            for (key, def) in own {
                let kind = if is_setter_key(&key) {
                    DynMemberKind::Setter
                } else if cx.fn_info(def).is_getter {
                    DynMemberKind::Getter
                } else {
                    DynMemberKind::Method
                };
                let name = key_member_name(&key).to_string();
                let setter = kind == DynMemberKind::Setter;
                if members
                    .iter()
                    .any(|m| m.name == name && (m.kind == DynMemberKind::Setter) == setter)
                {
                    continue;
                }
                members.push(DynMember {
                    name,
                    kind,
                    def,
                    owner,
                });
            }
            let Some(base) = base else { break };
            let base = cx.subst(base, &cur_args);
            let Some(next) = cx.class_of(base) else { break };
            (cur, cur_args) = next;
        }
        if cx.literal_structs.contains(&d) {
            literal_accessors(cx, d, &mut members);
        }
        let readonly: Vec<String> = cx
            .adt(d)
            .map(|a| {
                a.fields
                    .iter()
                    .filter(|f| f.readonly)
                    .map(|f| f.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let own_args = args_of(cx, d);
        let own = cx.ty.intern(TyKind::Adt(d, own_args));
        for name in readonly {
            members.push(DynMember {
                name,
                kind: DynMemberKind::ReadonlyField,
                def: d,
                owner: own,
            });
        }
        if !members.is_empty() {
            out.insert(d, members);
        }
    }
    Some(out)
}

/// Does the program convert values to `any` and make object literals with accessors in its own
/// modules (the standard library's are not printed or serialized through `any`)? Then
/// printing, `JSON.stringify` and `Object.keys` of an `any` that holds one (also inside
/// another object) need its accessors listed, even where no member is read through `any`.
fn accessor_literals_dynamic(cx: &Ctx) -> bool {
    if !cx.any.is_some_and(|a| a.imp.is_some()) {
        return false;
    }
    cx.literal_structs.iter().any(|&d| {
        cx.adt(d).is_some_and(|a| {
            !cx.scopes[a.module].is_std
                && (a.implements.iter()).any(|b| {
                    cx.iface(b.iface).is_some_and(|i| {
                        (i.methods.iter()).any(|m| m.is_getter || m.name.starts_with("set "))
                    })
                })
        })
    })
}

/// The getters and setters of an object literal that implements an interface
/// (`body::expr::iface_object`), whose struct `d` holds them as functions in its fields: the
/// implementation's slots for them, so a read or write through `any` (or a view of the
/// literal, #1510) runs the accessor rather than reaching the function in the field (#1538).
fn literal_accessors(cx: &mut Ctx, d: DefId, members: &mut Vec<DynMember>) {
    let own = args_of(cx, d);
    let owner = cx.ty.intern(TyKind::Adt(d, own));
    let found = cx.impls.iter().find(|i| i.ty == owner);
    let Some((iface, defs)) = found.map(|i| (i.iface, i.methods.clone())) else {
        return;
    };
    let Some(i) = cx.iface(iface) else {
        return;
    };
    let slots: Vec<(String, bool, TyId)> = (i.methods.iter())
        .map(|m| (m.name.clone(), m.is_getter, m.ret))
        .collect();
    // The struct's fields: a getter written `get k() { … }` is a function in field `k` (a
    // value `k: v` filling a getter slot stays a field), a setter one in field `set k`.
    let fields: Vec<(String, TyId)> = (cx.adt(d).into_iter())
        .flat_map(|a| a.fields.iter().map(|f| (f.name.clone(), f.ty)))
        .collect();
    let has_field = |n: &str| fields.iter().any(|(f, _)| f == n);
    for ((name, getter, ret), def) in slots.into_iter().zip(defs) {
        let (kind, name) = match name.strip_prefix("set ") {
            Some(n) if has_field(&name) => (DynMemberKind::Setter, n.to_string()),
            None if getter => {
                let held = fields.iter().find(|(f, _)| *f == name).map(|(_, t)| *t);
                let is_fn = |t: TyId| matches!(cx.ty.kind(t), TyKind::FnPtr { .. });
                match held {
                    Some(t) if is_fn(t) && t != ret => (DynMemberKind::Getter, name),
                    _ => continue,
                }
            }
            _ => continue,
        };
        let setter = kind == DynMemberKind::Setter;
        if members
            .iter()
            .any(|m| m.name == name && (m.kind == DynMemberKind::Setter) == setter)
        {
            continue;
        }
        members.push(DynMember {
            name,
            kind,
            def,
            owner,
        });
    }
}

/// The type parameters of `d` as types (`Param(0)`, ...).
fn args_of(cx: &mut Ctx, d: DefId) -> Vec<TyId> {
    let n = cx.adt(d).map_or(0, |a| a.generics.names.len()) as u32;
    (0..n).map(|k| cx.ty.intern(TyKind::Param(k))).collect()
}

impl Ctx<'_> {
    /// A member of an `any` is read or called: lowering adds the member slots
    /// (`Program::dyn_members`).
    pub(crate) fn note_dyn_members(&mut self) {
        if let Some(a) = &mut self.any {
            a.members = true;
        }
    }
}

//! Built-in methods of strings, arrays, numbers and booleans called through `any` (#924): the
//! `AnyCall` slot of such a type (any_glue.rs) compares the key with the names of the prelude's
//! functions for its kind (std/prelude/zzzz_any_methods.vlt: `__anyStr_<name>(s, args)`,
//! `__anyArr_<name><T>(a, args)` instantiated at the element type, `__anyNum_<name>(n, args)`,
//! `__anyBool_<name>(b, args)`) and calls the one named, with the value borrowed and the
//! arguments packed into an `any[]`; its `any` result is the call's. Another JavaScript
//! built-in is a member not supported yet (`ST_UNSUPPORTED`, a `TypeError` naming #924), and any
//! other name is not a function (`ST_NOT_FN`: Node's `x.foo is not a function`). Only the names the
//! program calls through `any` are compared (`Boxing::any_call_keys`), so a program that calls
//! none of them has the slots it had before.

use velt_sema::hir::{self, DefId, PassMode, TyId, TyKind};

use super::any_glue::Ready;
use super::any_member::{ST_NOT_FN, ST_OK, ST_UNSUPPORTED};
use super::FnLower;
use crate::vir::{self, Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// The name prefix of the prelude's methods for values of the concrete type `ty`, if it
    /// has built-in methods.
    pub(super) fn builtin_prefix(&self, ty: TyId) -> Option<&'static str> {
        match self.cx.kind(ty) {
            TyKind::Str => Some("__anyStr_"),
            // An array of nothing but `void` (an empty literal's type) has no element to hand out.
            TyKind::Array(e) if !matches!(self.cx.kind(e), TyKind::Unit | TyKind::Never) => {
                Some("__anyArr_")
            }
            TyKind::Int(_) | TyKind::Float(_) => Some("__anyNum_"),
            TyKind::Bool => Some("__anyBool_"),
            // A function has none here yet: `call`, `apply`, `bind` and `toString` are not
            // supported, and any other name is not a function.
            TyKind::FnPtr { .. } | TyKind::Closure(_) => Some("__anyFn_"),
            _ => None,
        }
    }

    /// The prelude's methods named `<prefix><name>`, by name, that take the value and an
    /// `any[]`.
    fn builtin_methods(&self, prefix: &str) -> Vec<(String, DefId)> {
        let mut out = vec![];
        for (i, d) in self.cx.hir.defs.iter().enumerate() {
            let hir::Def::Fn(f) = d else { continue };
            if !f.name.starts_with("std/prelude/") || f.params.len() != 2 {
                continue;
            }
            let Some((_, short)) = f.name.rsplit_once("::") else {
                continue;
            };
            let Some(name) = short.strip_prefix(prefix) else {
                continue;
            };
            out.push((name.to_string(), DefId(i as u32)));
        }
        out
    }

    /// The `AnyCall` slot body of a value of the concrete type `ty` with built-in methods
    /// (module docs).
    pub(super) fn any_builtin_call(
        &mut self,
        prefix: &str,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let keys = &self.cx.boxing.any_call_keys;
        let methods: Vec<(String, DefId)> = self
            .builtin_methods(prefix)
            .into_iter()
            .filter(|(name, _)| keys.contains("") || keys.contains(name))
            .collect();
        if keys.is_empty() {
            // No call through `any` names a member: the slot as before.
            self.any_drop_args(argv, 0, argc);
            return self.ret_status(ST_UNSUPPORTED);
        }
        // The JavaScript built-ins of this kind the program may call that are not here yet:
        // a `TypeError` naming #924; any other name is not a function, as in Node.
        let missing: Vec<&str> = js_methods(prefix)
            .iter()
            .chain(OBJECT_METHODS)
            .copied()
            .filter(|n| keys.contains("") || keys.contains(*n))
            .filter(|n| !methods.iter().any(|(m, _)| m == n))
            .collect();
        let outp = self.any_out(out);
        let any = self.cx.any_ty();
        let rest = self.cx.intern(TyKind::Array(any));
        let targs = match self.cx.kind(ty) {
            TyKind::Array(e) => vec![e],
            _ => vec![],
        };
        let f64_ = self.cx.intern(TyKind::Float(hir::FloatTy::F64));
        for (name, def) in methods {
            if self.cx.fn_def(def).generics as usize != targs.len() {
                continue;
            }
            let next = self.on_key(key, &name);
            let this = match self.cx.kind(ty) {
                // A number held at another width (`size` read through `any`) is a `number`.
                TyKind::Int(_) | TyKind::Float(_) if ty != f64_ => {
                    let vp = self.any_value_place(data, ty);
                    let from = self.cx.ty(ty);
                    self.cast_to(Operand::Copy(vp), from, Ty::F64)
                }
                // A method that keeps the value (passes it on as an `any`) takes a share of it.
                _ if self.cx.fn_def(def).params[0].mode == PassMode::Owned => {
                    let vp = self.any_value_place(data, ty);
                    let v = self.share_value(Operand::Copy(vp), ty);
                    let vt = self.cx.ty(ty);
                    let p = Place::local(self.copy_to_temp(v, vt));
                    self.take_temp(&p);
                    match vt {
                        Ty::Agg(_) => self.addr(p),
                        _ => Operand::Copy(p),
                    }
                }
                _ => self.any_this(data, ty),
            };
            let args = vec![self.any_rest_arg(argv, argc, 0, rest)];
            let r = Ready {
                def,
                targs: targs.clone(),
                ret: any,
            };
            self.any_invoke(&r, this, args, &outp);
            self.ret_status(ST_OK);
            self.switch_to(next);
        }
        for name in missing {
            let next = self.on_key(key, name);
            self.any_drop_args(argv, 0, argc);
            self.ret_status(ST_UNSUPPORTED);
            self.switch_to(next);
        }
        self.any_drop_args(argv, 0, argc);
        self.ret_status(ST_NOT_FN);
    }
}

/// The methods of JavaScript's `String`, `Array`, `Number`, `Boolean` and `Function`
/// prototypes.
fn js_methods(prefix: &str) -> &'static [&'static str] {
    match prefix {
        "__anyStr_" => &[
            "at",
            "charAt",
            "charCodeAt",
            "codePointAt",
            "concat",
            "endsWith",
            "includes",
            "indexOf",
            "isWellFormed",
            "lastIndexOf",
            "localeCompare",
            "match",
            "matchAll",
            "normalize",
            "padEnd",
            "padStart",
            "repeat",
            "replace",
            "replaceAll",
            "search",
            "slice",
            "split",
            "startsWith",
            "substr",
            "substring",
            "toLocaleLowerCase",
            "toLocaleUpperCase",
            "toLowerCase",
            "toString",
            "toUpperCase",
            "toWellFormed",
            "trim",
            "trimEnd",
            "trimLeft",
            "trimRight",
            "trimStart",
            "valueOf",
        ],
        "__anyArr_" => &[
            "at",
            "concat",
            "copyWithin",
            "entries",
            "every",
            "fill",
            "filter",
            "find",
            "findIndex",
            "findLast",
            "findLastIndex",
            "flat",
            "flatMap",
            "forEach",
            "includes",
            "indexOf",
            "join",
            "keys",
            "lastIndexOf",
            "map",
            "pop",
            "push",
            "reduce",
            "reduceRight",
            "reverse",
            "shift",
            "slice",
            "some",
            "sort",
            "splice",
            "toReversed",
            "toSorted",
            "toSpliced",
            "toString",
            "unshift",
            "values",
            "with",
        ],
        "__anyNum_" => &[
            "toExponential",
            "toFixed",
            "toPrecision",
            "toString",
            "valueOf",
        ],
        "__anyFn_" => &["apply", "bind", "call", "toString", "valueOf"],
        _ => &["toString", "valueOf"],
    }
}

/// The methods every value has from `Object.prototype`.
const OBJECT_METHODS: &[&str] = &[
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
];

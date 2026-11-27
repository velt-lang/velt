//! Built-in methods of strings, arrays, numbers and booleans called through `any` (#924): the
//! `AnyCall` slot of such a type (any_glue.rs) compares the key with the names of std's methods
//! for its kind, the hook table's rows of its receiver (velt_common `std_hooks::any_methods`:
//! std/lang/hooks/any_methods.velt's `anyStr_<name>(s, args)`, `anyArr_<name><T>(a, args)`
//! instantiated at the element type, `anyNum_<name>(n, args)`, `anyBool_<name>(b, args)`), and
//! calls the one named, with the value borrowed and the arguments packed into an `any[]`; its
//! `any` result is the call's. A row's module loaded without its function is an internal error
//! (`std_defs.rs`); its module not loaded, the method is not in the program. Another JavaScript
//! built-in is a member not supported yet (`ST_UNSUPPORTED`, a `TypeError` naming #924), and any
//! other name is not a function (`ST_NOT_FN`: Node's `x.foo is not a function`). Only the names the
//! program calls through `any` are compared (`Boxing::any_call_keys`), so a program that calls
//! none of them has the slots it had before.

use velt_common::std_hooks::any_methods::{methods, record_key};
use velt_common::std_hooks::Receiver;
use velt_sema::hir::{self, DefId, PassMode, TyId, TyKind};

use super::any_member::{ST_NOT_FN, ST_OK, ST_UNSUPPORTED};
use super::any_ready::Ready;
use super::FnLower;
use crate::vir::{self, Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// The receiver of std's methods for values of the concrete type `ty`, if it has built-in
    /// methods.
    pub(super) fn builtin_receiver(&self, ty: TyId) -> Option<Receiver> {
        match self.cx.kind(ty) {
            TyKind::Str => Some(Receiver::Str),
            // An array of nothing but `void` (an empty literal's type) has no element to hand out.
            TyKind::Array(e) if !matches!(self.cx.kind(e), TyKind::Unit | TyKind::Never) => {
                Some(Receiver::Arr)
            }
            TyKind::Int(_) | TyKind::Float(_) => Some(Receiver::Num),
            TyKind::Bool => Some(Receiver::Bool),
            // A function has none here yet: `call`, `apply`, `bind` and `toString` are not
            // supported, and any other name is not a function.
            TyKind::FnPtr { .. } | TyKind::Closure(_) => Some(Receiver::Fn),
            // A promise held as `any` (#924): `toString`; `then`, `catch` and `finally` come through
            // sema's own route (`any_promise_members.rs`).
            TyKind::Promise(..) => Some(Receiver::Promise),
            _ => None,
        }
    }

    /// std's methods of receiver `r` in the program, by name: the hook table's rows of `r`
    /// (velt_common `std_hooks::any_methods`) whose module the program loads.
    fn builtin_methods(&self, r: Receiver) -> Vec<(&'static str, DefId)> {
        methods(r)
            .filter_map(|(name, h)| Some((name, self.cx.std_hook_if_loaded(&h.qualified())?)))
            .collect()
    }

    /// The `AnyCall` slot body of a value of the concrete type `ty` with built-in methods
    /// (module docs).
    pub(super) fn any_builtin_call(
        &mut self,
        r: Receiver,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let keys = &self.cx.boxing.any_call_keys;
        let methods: Vec<(&str, DefId)> = self
            .builtin_methods(r)
            .into_iter()
            .filter(|(name, _)| keys.contains("") || keys.contains(*name))
            .collect();
        if keys.is_empty() {
            // No call through `any` names a member: the slot as before.
            self.any_drop_args(argv, 0, argc);
            return self.ret_status(ST_UNSUPPORTED);
        }
        // The JavaScript built-ins of this kind the program may call that are not here yet:
        // a `TypeError` naming #924; any other name is not a function, as in Node.
        let missing: Vec<&str> = js_methods(r)
            .iter()
            .chain(OBJECT_METHODS)
            .copied()
            .filter(|n| keys.contains("") || keys.contains(*n))
            .filter(|n| !methods.iter().any(|(m, _)| m == n))
            .collect();
        let outp = self.any_out(out);
        let targs = match self.cx.kind(ty) {
            TyKind::Array(e) => vec![e],
            TyKind::Promise(t, e) => vec![t, e],
            _ => vec![],
        };
        let f64_ = self.cx.intern(TyKind::Float(hir::FloatTy::F64));
        for (name, def) in methods {
            if self.cx.fn_def(def).generics as usize != targs.len() {
                continue;
            }
            let next = self.on_key(key, name);
            let this = match self.cx.kind(ty) {
                // A number held at another width (`size` read through `any`) is a `number`.
                TyKind::Int(_) | TyKind::Float(_) if ty != f64_ => {
                    let vp = self.any_value_place(data, ty);
                    match self.cx.kind(ty) {
                        TyKind::Float(_) => self.float_as_f64(Operand::Copy(vp), ty),
                        _ => {
                            let from = self.cx.ty(ty);
                            self.cast_to(Operand::Copy(vp), from, Ty::F64)
                        }
                    }
                }
                _ => self.builtin_this(data, ty, def),
            };
            self.call_builtin(def, &targs, this, (argv, argc), &outp, None);
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

impl FnLower<'_, '_> {
    /// The `AnyCall` slot body of a `Record<string, V>`: std's methods of a record, the hook
    /// table's `Receiver::Rec` rows (std/lang/record/reflect.velt's `anyRec_<name>`), each called
    /// for the key of std's symbol `record<Name>` (std/lang/hooks/keys.velt: `r[RECORD_SYM_GET](s)`,
    /// which no program's key reaches; `record_key`), given the record and the arguments; any
    /// other name is not supported yet, as before.
    pub(super) fn any_record_call(
        &mut self,
        (data, key, argv, argc, out): (vir::Local, vir::Local, vir::Local, vir::Local, vir::Local),
        ty: TyId,
    ) {
        let keys = &self.cx.boxing.any_call_keys;
        let methods: Vec<(String, DefId)> = self
            .builtin_methods(Receiver::Rec)
            .into_iter()
            .map(|(name, d)| (record_key(name), d))
            .filter(|(key, _)| keys.contains(key))
            .collect();
        let Some((k, v)) = self.prelude_record(ty) else {
            crate::lower::ice("a record");
        };
        if !matches!(self.cx.kind(k), TyKind::Str) {
            self.any_drop_args(argv, 0, argc);
            return self.ret_status(ST_UNSUPPORTED);
        }
        let outp = self.any_out(out);
        let any = self.cx.any_ty();
        let rest = self.cx.intern(TyKind::Array(any));
        for (name, def) in methods {
            if self.cx.fn_def(def).generics != 1 {
                continue;
            }
            let next = self.on_key(key, &name);
            let this = self.any_this(data, ty);
            let args = vec![self.any_rest_arg(argv, argc, 0, rest)];
            let r = Ready {
                def,
                targs: vec![v],
                ret: any,
                throws: None,
            };
            self.any_invoke(&r, this, args, &outp);
            self.ret_status(ST_OK);
            self.switch_to(next);
        }
        self.any_record_value_call((data, key, argv, argc, out), ty);
    }

    /// `this` for the standard library's method `def` on the value of type `ty` that `data`
    /// holds: a share when the method keeps it (passes it on as an `any`), else borrowed.
    fn builtin_this(&mut self, data: vir::Local, ty: TyId, def: DefId) -> Operand {
        if self.cx.fn_def(def).params[0].mode != PassMode::Owned {
            return self.any_this(data, ty);
        }
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

    /// Call `def(this, args)`, the arguments packed into an `any[]`; its `any` result is the
    /// call's (`ST_OK`). A `def` with a third parameter takes the receiver as an `any` of its
    /// own type `whole` (a user class extending `Map`, with its own members), held at `data`.
    fn call_builtin(
        &mut self,
        def: DefId,
        targs: &[TyId],
        this: Operand,
        (argv, argc): (vir::Local, vir::Local),
        outp: &Place,
        whole: Option<(vir::Local, TyId)>,
    ) {
        let any = self.cx.any_ty();
        let rest = self.cx.intern(TyKind::Array(any));
        let mut args = vec![self.any_rest_arg(argv, argc, 0, rest)];
        if let (Some((data, ty)), 3) = (whole, self.cx.fn_def(def).params.len()) {
            let vp = self.any_value_place(data, ty);
            let v = self.share_value(Operand::Copy(vp), ty);
            let avt = self.cx.ty(any);
            let slot = Place::local(self.temp(avt));
            self.any_box_with(v, ty, &slot, false);
            args.push((slot, any));
        }
        let r = Ready {
            def,
            targs: targs.to_vec(),
            ret: any,
            throws: None,
        };
        self.any_invoke(&r, this, args, outp);
        self.ret_status(ST_OK);
    }

    /// The methods of a standard library class `d` (a `Map` or `Set`, type `ty`) that glue
    /// cannot call as declared (`forEach`, whose callback has a `throws` type parameter): the
    /// hook table's rows of the class's receiver (`any<Class>_<name>` taking the value and an
    /// `any[]`), instantiated at the class's type arguments, for the names the program calls
    /// through `any`. Continues in the block for other names.
    pub(super) fn any_class_call(
        &mut self,
        (data, key, argv, argc): (vir::Local, vir::Local, vir::Local, vir::Local),
        (ty, d): (TyId, DefId),
        outp: &Place,
    ) {
        if self.cx.boxing.any_call_keys.is_empty() {
            return;
        }
        // A user class extending `Map` or `Set` (`class MyMap extends Map<string, number>`):
        // the standard class it extends, at the type arguments it gives it.
        let (mut base, mut bd) = (ty, d);
        let class = loop {
            let (name, b) = match &self.cx.hir.defs[bd.0 as usize] {
                hir::Def::Adt(a) => (a.name.clone(), a.base),
                _ => return,
            };
            if name.starts_with(velt_common::std_modules::CANONICAL_PREFIX) {
                match name.rsplit_once("::") {
                    Some((_, class)) => break class.to_string(),
                    None => return,
                }
            }
            let (Some(b), Some((_, args))) = (b, self.plain_adt(base)) else {
                return;
            };
            base = self.cx.subst(b, &args);
            let Some((next, _)) = self.plain_adt(base) else {
                return;
            };
            bd = next;
        };
        let Some(r) = Receiver::of_class(&class) else {
            return;
        };
        let keys = &self.cx.boxing.any_call_keys;
        let methods: Vec<(&str, DefId)> = (self.builtin_methods(r).into_iter())
            .filter(|(name, _)| keys.contains("") || keys.contains(*name))
            .collect();
        let Some((_, targs)) = self.plain_adt(base) else {
            return;
        };
        for (name, def) in methods {
            if self.cx.fn_def(def).generics as usize != targs.len() {
                continue;
            }
            let next = self.on_key(key, name);
            let this = self.builtin_this(data, ty, def);
            self.call_builtin(def, &targs, this, (argv, argc), outp, Some((data, ty)));
            self.switch_to(next);
        }
    }
}

/// The methods of JavaScript's `String`, `Array`, `Number`, `Boolean` and `Function`
/// prototypes.
fn js_methods(r: Receiver) -> &'static [&'static str] {
    match r {
        Receiver::Str => &[
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
        Receiver::Arr => &[
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
        Receiver::Num => &[
            "toExponential",
            "toFixed",
            "toPrecision",
            "toString",
            "valueOf",
        ],
        Receiver::Fn => &["apply", "bind", "call", "toString", "valueOf"],
        Receiver::Promise => &["catch", "finally", "then", "toString", "valueOf"],
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

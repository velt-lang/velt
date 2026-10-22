//! Type interning helpers, classification, substitution of generic parameters and
//! pattern matching of generic signatures against concrete types (type-argument inference).

use crate::hir::{FloatTy, IntTy, Nothing, TyId, TyKind, TyTable};

pub(crate) struct Types {
    pub table: TyTable,
    pub unit: TyId,
    pub never: TyId,
    pub error: TyId,
    pub bool_: TyId,
    pub str_: TyId,
    pub i32: TyId,
    pub i64: TyId,
    pub u64: TyId,
    pub usize: TyId,
    pub f64: TyId,
    /// The program names the type `bigint` (`never` until `BigInt` values exist, #1088).
    pub bigint_named: bool,
    /// The function types of sites resolved with an inferred error type, and the types made
    /// from them by substitution: shown as written, without it (`throws::fn_types`).
    pub inferred_fns: rustc_hash::FxHashSet<TyId>,
}

impl Types {
    pub fn new() -> Self {
        let mut table = TyTable::new();
        let unit = table.intern(TyKind::Unit);
        let never = table.intern(TyKind::Never);
        let error = table.intern(TyKind::Error);
        let bool_ = table.intern(TyKind::Bool);
        let str_ = table.intern(TyKind::Str);
        let i32 = table.intern(TyKind::Int(IntTy::I32));
        let i64 = table.intern(TyKind::Int(IntTy::I64));
        let u64 = table.intern(TyKind::Int(IntTy::U64));
        let usize = table.intern(TyKind::Int(IntTy::USize));
        let f64 = table.intern(TyKind::Float(FloatTy::F64));
        Types {
            table,
            unit,
            never,
            error,
            bool_,
            str_,
            i32,
            i64,
            u64,
            usize,
            f64,
            bigint_named: false,
            inferred_fns: Default::default(),
        }
    }

    /// The type `bigint` names: `never`, since no value is a `bigint` yet (#1088).
    pub fn bigint(&mut self) -> TyId {
        self.bigint_named = true;
        self.never
    }

    /// The id of type `k`. A function type whose rest parameter is a tuple
    /// (`(...args: [string, number]) => R`, often a type parameter's or `infer`'s value) is the
    /// function type with those parameters (`(a: string, b: number) => R`), as in TypeScript.
    pub fn intern(&mut self, k: TyKind) -> TyId {
        if let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest: true,
        } = &k
        {
            if let Some(TyKind::Tuple(ts)) = params.last().map(|t| self.kind(*t)) {
                let mut params = params[..params.len() - 1].to_vec();
                params.extend(ts);
                let (ret, throws) = (*ret, *throws);
                return self.table.intern(TyKind::FnPtr {
                    params,
                    ret,
                    throws,
                    rest: false,
                });
            }
        }
        self.table.intern(k)
    }

    /// `to`, a function type made from `from` by replacing its parts, is shown as written when
    /// `from` is ([`Types::inferred_fns`]).
    pub fn inferred_like(&mut self, from: TyId, to: TyId) -> TyId {
        if from != to && !self.inferred_fns.is_empty() && self.inferred_fns.contains(&from) {
            self.inferred_fns.insert(to);
        }
        to
    }

    pub fn kind(&self, t: TyId) -> &TyKind {
        self.table.kind(t)
    }

    pub fn int_ty(&self, t: TyId) -> Option<IntTy> {
        match self.kind(t) {
            TyKind::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn is_int(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Int(_))
    }

    pub fn is_float(&self, t: TyId) -> bool {
        matches!(self.kind(t), TyKind::Float(_))
    }

    pub fn is_numeric(&self, t: TyId) -> bool {
        self.is_int(t) || self.is_float(t)
    }

    /// `Error` or `Never`: compatible with anything, never the source of a new diagnostic.
    pub fn is_bottom(&self, t: TyId) -> bool {
        t == self.error || t == self.never
    }

    /// `t | null`
    pub fn option(&mut self, t: TyId) -> TyId {
        self.intern(TyKind::Option(t, Nothing::Null))
    }

    /// `t | null`, `t | undefined` or both, per `n`.
    pub fn option_of(&mut self, t: TyId, n: Nothing) -> TyId {
        self.intern(TyKind::Option(t, n))
    }

    /// Which nothings option type `t` allows (`None` if `t` is not an option).
    pub fn nothing_of(&self, t: TyId) -> Option<Nothing> {
        match self.kind(t) {
            TyKind::Option(_, n) => Some(*n),
            _ => None,
        }
    }

    pub fn array(&mut self, t: TyId) -> TyId {
        self.intern(TyKind::Array(t))
    }

    pub fn param(&mut self, n: u32) -> TyId {
        self.intern(TyKind::Param(n))
    }

    /// Payload of `T | null` (or `T | undefined`): what `WrapSome` / `UnwrapSome` hold. For
    /// `T | null | undefined` that is `T | null` (`TyTable::payload`); `core` is `T` itself.
    pub fn opt_payload(&self, t: TyId) -> Option<TyId> {
        self.table.payload(t)
    }

    /// `undefined` alone as a type: an option of `never`, so its one value is the option's
    /// nothing (hir_encodings.md "Undefined").
    pub fn undefined(&mut self) -> TyId {
        let never = self.never;
        self.option_of(never, Nothing::Undefined)
    }

    /// `null` alone as a type (a field that is always `null` in one member of a discriminated
    /// union): an option of `never`, like `undefined` alone.
    pub fn null_alone(&mut self) -> TyId {
        let never = self.never;
        self.option_of(never, Nothing::Null)
    }

    /// `T` of `T | null`, `T | undefined` and `T | null | undefined`.
    pub fn core(&self, t: TyId) -> Option<TyId> {
        match self.kind(t) {
            TyKind::Option(p, _) => Some(*p),
            _ => None,
        }
    }

    /// `Promise<t>` that never rejects.
    pub fn promise(&mut self, t: TyId) -> TyId {
        let never = self.never;
        self.intern(TyKind::Promise(t, never))
    }

    /// `Promise<t, e>`: resolves to `t` or rejects with `e`.
    pub fn promise_rejecting(&mut self, t: TyId, e: TyId) -> TyId {
        self.intern(TyKind::Promise(t, e))
    }

    /// `T` of `Promise<T, E>`.
    pub fn promise_payload(&self, t: TyId) -> Option<TyId> {
        match self.kind(t) {
            TyKind::Promise(p, _) => Some(*p),
            _ => None,
        }
    }

    /// `E` of `Promise<T, E>` (`never` when it cannot reject).
    pub fn promise_error(&self, t: TyId) -> Option<TyId> {
        match self.kind(t) {
            TyKind::Promise(_, e) => Some(*e),
            _ => None,
        }
    }

    /// A non-throwing function type.
    pub fn fn_ptr(&mut self, params: Vec<TyId>, ret: TyId) -> TyId {
        let throws = self.never;
        self.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
            rest: false,
        })
    }

    /// What an async function with declared return type `t` returns from its body.
    pub fn async_result(&self, t: TyId) -> TyId {
        self.promise_payload(t).unwrap_or(t)
    }

    pub fn array_elem(&self, t: TyId) -> Option<TyId> {
        match self.kind(t) {
            TyKind::Array(e) => Some(*e),
            _ => None,
        }
    }

    /// Does `t` mention `Error` anywhere (an unknown / failed part)?
    pub fn has_error(&self, t: TyId) -> bool {
        let mut found = false;
        self.visit(t, &mut |k| found |= matches!(k, TyKind::Error));
        found
    }

    pub(crate) fn visit(&self, t: TyId, f: &mut dyn FnMut(&TyKind)) {
        let k = self.kind(t);
        f(k);
        for c in children(k) {
            self.visit(c, f);
        }
    }

    /// Replace `Param(i)` by `args[i]` (params beyond `args` are kept).
    pub fn subst(&mut self, t: TyId, args: &[TyId]) -> TyId {
        if args.is_empty() {
            return t;
        }
        self.map(t, &mut |k| match k {
            TyKind::Param(i) => args.get(*i as usize).copied(),
            _ => None,
        })
    }

    /// Like [`Types::subst`] with partially known args; unknown ones become `Error`
    /// ("no constraint" when used as an expected type).
    pub fn subst_known(&mut self, t: TyId, slots: &[Option<TyId>]) -> TyId {
        let error = self.error;
        self.map(t, &mut |k| match k {
            TyKind::Param(i) => slots.get(*i as usize).map(|s| s.unwrap_or(error)),
            _ => None,
        })
    }

    /// Does `t` mention `Error` outside its error types (what a promise rejects with, what a
    /// function type throws)?
    pub fn has_error_outside_error_types(&self, t: TyId) -> bool {
        let any = |ts: &[TyId]| ts.iter().any(|x| self.has_error_outside_error_types(*x));
        match self.kind(t) {
            TyKind::Error => true,
            TyKind::Promise(v, _) => self.has_error_outside_error_types(*v),
            TyKind::FnPtr { params, ret, .. } => any(params) || any(&[*ret]),
            TyKind::Adt(_, args)
            | TyKind::Dyn(_, args)
            | TyKind::Tuple(args)
            | TyKind::Op(_, args) => any(args),
            TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => any(&[*e]),
            TyKind::Map(a, b) | TyKind::Result(a, b) => any(&[*a, *b]),
            _ => false,
        }
    }

    /// `t` with every error type (of a promise, thrown by a function type) made `Error`, i.e.
    /// unconstrained: `Promise<T>` as a contextual type says nothing about what it rejects with.
    pub fn without_error_types(&mut self, t: TyId) -> TyId {
        let error = self.error;
        let k = self.kind(t).clone();
        let m = |s: &mut Self, x: TyId| s.without_error_types(x);
        let nk = match k {
            TyKind::Promise(v, _) => TyKind::Promise(m(self, v), error),
            TyKind::FnPtr {
                params, ret, rest, ..
            } => TyKind::FnPtr {
                params: params.iter().map(|a| m(self, *a)).collect(),
                ret: m(self, ret),
                throws: error,
                rest,
            },
            TyKind::Adt(d, args) => TyKind::Adt(d, args.iter().map(|a| m(self, *a)).collect()),
            TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|a| m(self, *a)).collect()),
            TyKind::Array(e) => TyKind::Array(m(self, e)),
            TyKind::Map(a, b) => TyKind::Map(m(self, a), m(self, b)),
            TyKind::Tuple(ts) => TyKind::Tuple(ts.iter().map(|a| m(self, *a)).collect()),
            TyKind::Option(e, n) => TyKind::Option(m(self, e), n),
            TyKind::Result(a, b) => TyKind::Result(m(self, a), m(self, b)),
            TyKind::Shared(e) => TyKind::Shared(m(self, e)),
            _ => return t,
        };
        self.intern(nk)
    }

    /// Rebuild `t` bottom-up; `f` may replace a node (returning `Some`).
    pub fn map(&mut self, t: TyId, f: &mut dyn FnMut(&TyKind) -> Option<TyId>) -> TyId {
        let k = self.kind(t).clone();
        if let Some(r) = f(&k) {
            return r;
        }
        let mut m = |s: &mut Self, x: TyId| s.map(x, f);
        let nk = match k {
            TyKind::Adt(d, args) => TyKind::Adt(d, args.iter().map(|a| m(self, *a)).collect()),
            TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|a| m(self, *a)).collect()),
            TyKind::Op(o, args) => TyKind::Op(o, args.iter().map(|a| m(self, *a)).collect()),
            TyKind::Array(e) => TyKind::Array(m(self, e)),
            TyKind::Map(a, b) => TyKind::Map(m(self, a), m(self, b)),
            TyKind::Tuple(ts) => TyKind::Tuple(ts.iter().map(|a| m(self, *a)).collect()),
            TyKind::Option(e, n) => TyKind::Option(m(self, e), n),
            TyKind::Result(a, b) => TyKind::Result(m(self, a), m(self, b)),
            TyKind::Promise(v, e) => TyKind::Promise(m(self, v), m(self, e)),
            TyKind::Shared(e) => TyKind::Shared(m(self, e)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => TyKind::FnPtr {
                params: params.iter().map(|a| m(self, *a)).collect(),
                ret: m(self, ret),
                throws: m(self, throws),
                rest,
            },
            other => return self.intern(other),
        };
        let out = self.intern(nk);
        self.inferred_like(t, out)
    }

    /// Resolve a primitive type name (`i64`, `number`, `string`, ...).
    pub fn primitive(&mut self, name: &str) -> Option<TyId> {
        let k = match name {
            "i8" => TyKind::Int(IntTy::I8),
            "i16" => TyKind::Int(IntTy::I16),
            "i32" => TyKind::Int(IntTy::I32),
            "i64" => TyKind::Int(IntTy::I64),
            "isize" => TyKind::Int(IntTy::ISize),
            "u8" => TyKind::Int(IntTy::U8),
            "u16" => TyKind::Int(IntTy::U16),
            "u32" => TyKind::Int(IntTy::U32),
            "u64" => TyKind::Int(IntTy::U64),
            "usize" => TyKind::Int(IntTy::USize),
            "f32" => TyKind::Float(FloatTy::F32),
            "f64" | "number" => TyKind::Float(FloatTy::F64),
            // `boolean` is TypeScript's name, so code can be shared with `tsc` (#353).
            "bool" | "boolean" => TyKind::Bool,
            "string" => TyKind::Str,
            "symbol" | "unique symbol" => TyKind::Symbol,
            "void" => TyKind::Unit,
            "never" => TyKind::Never,
            _ => TyKind::Simd(crate::hir::SimdTy::named(name)?),
        };
        Some(self.intern(k))
    }
}

/// Is `name` one of the built-in type names TypeScript does not have (`i8`..`u64`, `isize`,
/// `usize`, `f32`, `f64`, `bool`)? A declaration of the program may use it (#1074).
pub(crate) fn velt_only_type_name(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
    )
}

/// Direct component types of a type.
pub(crate) fn children(k: &TyKind) -> Vec<TyId> {
    match k {
        TyKind::Adt(_, a) | TyKind::Dyn(_, a) | TyKind::Tuple(a) | TyKind::Op(_, a) => a.clone(),
        TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => vec![*e],
        TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => vec![*a, *b],
        TyKind::FnPtr {
            params,
            ret,
            throws,
            ..
        } => {
            let mut v = params.clone();
            v.push(*ret);
            v.push(*throws);
            v
        }
        _ => vec![],
    }
}

/// `k` with each direct component type mapped by `f` (in [`children`]'s order).
pub(crate) fn map_children(k: &TyKind, f: &mut dyn FnMut(TyId) -> TyId) -> TyKind {
    fn m(ts: &[TyId], f: &mut dyn FnMut(TyId) -> TyId) -> Vec<TyId> {
        ts.iter().map(|t| f(*t)).collect()
    }
    match k {
        TyKind::Adt(d, a) => TyKind::Adt(*d, m(a, f)),
        TyKind::Dyn(d, a) => TyKind::Dyn(*d, m(a, f)),
        TyKind::Tuple(a) => TyKind::Tuple(m(a, f)),
        TyKind::Op(o, a) => TyKind::Op(*o, m(a, f)),
        TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } => {
            let params = m(params, f);
            TyKind::FnPtr {
                params,
                ret: f(*ret),
                throws: f(*throws),
                rest: *rest,
            }
        }
        TyKind::Array(e) => TyKind::Array(f(*e)),
        TyKind::Option(e, n) => TyKind::Option(f(*e), *n),
        TyKind::Shared(e) => TyKind::Shared(f(*e)),
        TyKind::Map(a, b) => TyKind::Map(f(*a), f(*b)),
        TyKind::Result(a, b) => TyKind::Result(f(*a), f(*b)),
        TyKind::Promise(a, b) => TyKind::Promise(f(*a), f(*b)),
        k => k.clone(),
    }
}

/// The generic parameters `t` mentions, in order of first occurrence (appended to `out`).
pub(crate) fn collect_params(ty: &Types, t: TyId, out: &mut Vec<u32>) {
    if let TyKind::Param(i) = ty.kind(t) {
        if !out.contains(i) {
            out.push(*i);
        }
        return;
    }
    for c in children(ty.kind(t)) {
        collect_params(ty, c, out);
    }
}

pub(crate) fn int_name(i: IntTy) -> &'static str {
    match i {
        IntTy::I8 => "i8",
        IntTy::I16 => "i16",
        IntTy::I32 => "i32",
        IntTy::I64 => "i64",
        IntTy::ISize => "isize",
        IntTy::U8 => "u8",
        IntTy::U16 => "u16",
        IntTy::U32 => "u32",
        IntTy::U64 => "u64",
        IntTy::USize => "usize",
    }
}

/// Largest literal magnitude allowed for `i` (`negated`: the literal is the operand of unary `-`).
pub(crate) fn int_max(i: IntTy, negated: bool) -> u128 {
    let bits = i.bits();
    if i.is_signed() {
        let m = (1u128 << (bits - 1)) - 1;
        if negated {
            m + 1
        } else {
            m
        }
    } else {
        (1u128 << bits) - 1
    }
}

pub(crate) fn int_range(i: IntTy) -> String {
    let bits = i.bits();
    if i.is_signed() {
        let m = 1i128 << (bits - 1);
        format!("{}..={}", -m, m - 1)
    } else {
        format!("0..={}", (1u128 << bits) - 1)
    }
}

/// A property name in a displayed object type, as TypeScript writes it: bare when it is an
/// identifier, otherwise quoted (`{ "content-type": string }`).
pub(crate) fn display_key(name: &str) -> String {
    let mut chars = name.chars();
    let ident = chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    if ident {
        name.to_string()
    } else {
        format!("{name:?}")
    }
}

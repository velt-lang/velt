//! Type interning helpers, classification, substitution of generic parameters and
//! pattern matching of generic signatures against concrete types (type-argument inference).

use crate::hir::{FloatTy, IntTy, TyId, TyKind, TyTable};

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
        }
    }

    pub fn intern(&mut self, k: TyKind) -> TyId {
        self.table.intern(k)
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

    pub fn option(&mut self, t: TyId) -> TyId {
        self.intern(TyKind::Option(t))
    }

    pub fn array(&mut self, t: TyId) -> TyId {
        self.intern(TyKind::Array(t))
    }

    pub fn param(&mut self, n: u32) -> TyId {
        self.intern(TyKind::Param(n))
    }

    /// Payload of `T | null`.
    pub fn opt_payload(&self, t: TyId) -> Option<TyId> {
        match self.kind(t) {
            TyKind::Option(p) => Some(*p),
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

    fn visit(&self, t: TyId, f: &mut dyn FnMut(&TyKind)) {
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
            TyKind::Array(e) => TyKind::Array(m(self, e)),
            TyKind::Map(a, b) => TyKind::Map(m(self, a), m(self, b)),
            TyKind::Tuple(ts) => TyKind::Tuple(ts.iter().map(|a| m(self, *a)).collect()),
            TyKind::Option(e) => TyKind::Option(m(self, e)),
            TyKind::Result(a, b) => TyKind::Result(m(self, a), m(self, b)),
            TyKind::Promise(v, e) => TyKind::Promise(m(self, v), m(self, e)),
            TyKind::Shared(e) => TyKind::Shared(m(self, e)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } => TyKind::FnPtr {
                params: params.iter().map(|a| m(self, *a)).collect(),
                ret: m(self, ret),
                throws: m(self, throws),
            },
            other => return self.intern(other),
        };
        self.intern(nk)
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
            "bool" => TyKind::Bool,
            "string" => TyKind::Str,
            "void" => TyKind::Unit,
            "never" => TyKind::Never,
            _ => return None,
        };
        Some(self.intern(k))
    }
}

/// Direct component types of a type.
pub(crate) fn children(k: &TyKind) -> Vec<TyId> {
    match k {
        TyKind::Adt(_, a) | TyKind::Dyn(_, a) | TyKind::Tuple(a) => a.clone(),
        TyKind::Array(e) | TyKind::Option(e) | TyKind::Shared(e) => vec![*e],
        TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => vec![*a, *b],
        TyKind::FnPtr {
            params,
            ret,
            throws,
        } => {
            let mut v = params.clone();
            v.push(*ret);
            v.push(*throws);
            v
        }
        _ => vec![],
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

//! A C call that passes or returns C structs by value (#2187 layer 3,
//! docs/internals/design/c-ffi.md "C structs"): the arguments as c_call.rs evaluates them, then
//! expanded as the target's C calling convention classifies the signature (`c_struct_abi`):
//!
//! - **In registers**: the struct is copied into a slot padded to whole eightbytes, whose words
//!   (`I64` or `F64`) are the arguments, so no byte past the struct is read.
//! - **On the stack**: a pointer to a copy, marked `CParam::ByVal`: the callee gets its own.
//! - **A result in registers**: an `Agg` of its one or two words is the extern's result
//!   (`CRet::Words`), which the call writes whole and which is read back as the struct. (Writing a
//!   word into a slot field and reading the slot as the struct would hide the write from the
//!   optimizer, which keeps the fields of one aggregate apart from another's view of it.)
//! - **A result in memory**: the caller's slot, passed first (`CParam::Sret`).
//!
//! A struct that arrives from C (the result) has its non-null pointer fields checked, as
//! `Ptr.read` does (ptr_memory.rs). Targets without a classification here are an error at the
//! declaration.

use velt_sema::hir::{self, CTy, DefId, TyId};

use super::c_call::{base_symbol, shown_signature, CDecl, CSig};
use super::{ice, unit, FnLower};
use crate::c_struct_abi::{self, Arg, Pass, Ret, StructShape};
use crate::vir::{self, AggId, CAbi, CParam, CRet, Operand, Place, Proj, Rvalue, Ty};

/// The C function a [`FnLower::c_call_by_value`] calls.
pub(super) struct StructCall<'a> {
    pub def: DefId,
    pub symbol: &'a str,
    /// The declaration's result type.
    pub ret: TyId,
    pub span: velt_common::Span,
}

impl FnLower<'_, '_> {
    /// The call of `call` (C signature `c`) with the arguments c_call.rs evaluated: their VIR
    /// types (`Unit` for a struct), the operands, and each struct argument's type.
    pub(super) fn c_call_by_value(
        &mut self,
        call: StructCall,
        c: &hir::CImport,
        (ctys, cargs, structs): (Vec<Ty>, Vec<Operand>, Vec<Option<TyId>>),
    ) -> Operand {
        let ret_struct = (c.ret == CTy::Struct).then(|| self.sub(call.ret));
        let ret_vt = match ret_struct {
            Some(t) => self.cx.ty(t),
            None => super::c_call::c_vir(c.ret, false),
        };
        let dummy = |me: &mut Self| match ret_vt {
            Ty::Unit => unit(),
            t => Operand::Copy(Place::local(me.temp(t))),
        };
        if !self.by_value_supported(&call, c) {
            return dummy(self);
        }
        let shapes: Vec<Arg> = (ctys.iter().zip(&structs))
            .map(|(&t, s)| match s {
                Some(st) => Arg::Struct(self.struct_shape(*st)),
                None => Arg::Scalar(t),
            })
            .collect();
        let ret_shape = ret_struct.map(|t| self.struct_shape(t));
        let plan = c_struct_abi::sysv(&shapes, ret_shape.as_ref());
        let (mut vparams, mut cparams, mut vargs) = (vec![], vec![], vec![]);
        let sret = match plan.ret {
            Ret::Sret { size, align } => {
                let slot = self.temp(ret_vt);
                vparams.push(Ty::Ptr);
                cparams.push(CParam::Sret { size, align });
                vargs.push(self.addr(Place::local(slot)));
                Some(slot)
            }
            _ => None,
        };
        for (k, pass) in plan.args.iter().enumerate() {
            let arg = cargs[k].clone();
            match pass {
                Pass::Value => {
                    vparams.push(ctys[k]);
                    cparams.push(CParam::Value);
                    vargs.push(arg);
                }
                Pass::Words(words) => {
                    let st = structs[k].unwrap_or_else(|| ice("words of a scalar argument"));
                    let vt = self.cx.ty(st);
                    let slot = self.words_slot(words);
                    let Ty::Agg(sagg) = vt else {
                        ice("a C struct that isn't an aggregate")
                    };
                    // Zeroed first: a struct smaller than its words (or with tail padding)
                    // leaves bytes of them unwritten, which would cross as undefined bits.
                    let zeros = (words.iter())
                        .map(|&w| match w {
                            Ty::F64 => Operand::Const(vir::Const::Float(0.0), Ty::F64),
                            t => Operand::Const(vir::Const::Int(0), t),
                        })
                        .collect();
                    let wagg = self.words_agg(words);
                    self.assign(Place::local(slot), Rvalue::Aggregate(wagg, zeros));
                    let view = Place {
                        local: slot,
                        proj: vec![Proj::Cast(sagg)],
                    };
                    self.assign(view, Rvalue::Use(arg));
                    for (i, &w) in words.iter().enumerate() {
                        vparams.push(w);
                        cparams.push(CParam::Value);
                        vargs.push(Operand::Copy(Place {
                            local: slot,
                            proj: vec![Proj::Field(i as u32)],
                        }));
                    }
                }
                &Pass::ByVal { size, align } => {
                    let st = structs[k].unwrap_or_else(|| ice("a scalar argument by value"));
                    let vt = self.cx.ty(st);
                    let copy = self.temp(vt);
                    self.assign(Place::local(copy), Rvalue::Use(arg));
                    vparams.push(Ty::Ptr);
                    cparams.push(CParam::ByVal { size, align });
                    vargs.push(self.addr(Place::local(copy)));
                }
            }
        }
        let (vret, cret) = match &plan.ret {
            Ret::Value => (ret_vt, CRet::Value),
            Ret::Words(w) => (Ty::Agg(self.words_agg(w)), CRet::Words),
            Ret::Sret { .. } => (Ty::Unit, CRet::Value),
        };
        let abi = CAbi {
            params: cparams,
            ret: cret,
        };
        let link = base_symbol(call.symbol);
        if let Some((ps, r, _)) = self.cx.extern_signature(link) {
            let same_abi = (self.cx.extern_map.get(link))
                .is_some_and(|id| self.cx.externs[id.0 as usize].c_abi.as_ref() == Some(&abi));
            if ps != vparams.as_slice() || r != vret || !same_abi {
                let (shown, sig) = (shown_signature(call.symbol, c), CSig::of(c));
                self.c_clash(call.def, (link, &shown, &sig), call.span);
                return dummy(self);
            }
        }
        let decl = (self.cx.locs.as_ref())
            .and_then(|m| m.loc(call.span).map(|l| m.describe(l)))
            .unwrap_or_else(|| link.to_string());
        self.cx.c_symbols.entry(link.to_string()).or_insert(CDecl {
            span: call.span,
            at: decl.clone(),
            shown: shown_signature(call.symbol, c),
            sig: CSig::of(c),
        });
        let id = self.cx.extern_sym_c(link, vparams, vret, false, Some(abi));
        self.cx.externs[id.0 as usize].c_decl = Some(decl);
        let dest = (vret != Ty::Unit).then(|| self.temp(vret));
        self.call(
            vir::Callee::Extern(id),
            vargs,
            dest.map(Place::local),
            false,
        );
        if c.errno {
            self.capture_errno(&c.library);
        }
        let Some(st) = ret_struct else {
            let Some(d) = dest else { return unit() };
            let result = Operand::Copy(Place::local(d));
            if c.ret == (CTy::Ptr { nullable: false }) {
                self.stop_if_null(result.clone(), &format!(
                    "C function `{link}` returned NULL, but its declaration's result is `Ptr<T>`: declare it `Ptr<T> | null`"
                ));
            }
            return result;
        };
        let Ty::Agg(sagg) = ret_vt else {
            ice("a C struct that isn't an aggregate")
        };
        let place = match (&plan.ret, sret, dest) {
            (Ret::Sret { .. }, Some(slot), _) => Place::local(slot),
            (Ret::Words(_), _, Some(d)) => Place {
                local: d,
                proj: vec![Proj::Cast(sagg)],
            },
            _ => ice("a C struct result without a place"),
        };
        let s = self.cx.type_name(st);
        self.stop_on_null(&place, st, (&s, Some(link)), "");
        let out = self.temp(ret_vt);
        self.assign(Place::local(out), Rvalue::Use(Operand::Copy(place)));
        Operand::Copy(Place::local(out))
    }

    /// Whether C structs by value cross on this target, and in this declaration; an error at
    /// the declaration (once) if not.
    fn by_value_supported(&mut self, call: &StructCall, c: &hir::CImport) -> bool {
        let why = match (c_struct_abi::supported(&self.cx.target), c.variadic) {
            (true, false) => return true,
            (false, _) => format!(
                "is not supported on {} yet: x86-64 Linux and macOS pass them now, the other targets' conventions come in later layers of #2187",
                self.cx.target
            ),
            (true, true) => "is not supported in a variadic C function yet".to_string(),
        };
        if self.cx.c_clashes_reported.insert(call.def) {
            self.cx.c_clashes.push(
                velt_common::Diagnostic::error(
                    format!(
                        "C function `{}` takes or returns a C struct by value, which {why}",
                        base_symbol(call.symbol)
                    ),
                    call.span,
                )
                .with_note("a `Ptr<S>` to the struct crosses on every target"),
            );
        }
        false
    }

    /// C struct `t` flattened for the classification: every scalar field at its offset.
    fn struct_shape(&mut self, t: TyId) -> StructShape {
        let Ty::Agg(a) = self.cx.ty(t) else {
            ice("a C struct that isn't an aggregate")
        };
        let mut fields = vec![];
        flatten(&self.cx.aggs, a, 0, &mut fields);
        let l = &self.cx.aggs[a.0 as usize];
        StructShape {
            fields,
            size: l.size,
            align: l.align,
        }
    }

    /// The aggregate of `words` at 0 and 8: what a struct crosses in, padded to whole eightbytes.
    fn words_agg(&mut self, words: &[Ty]) -> AggId {
        if let Some(&a) = self.cx.lay.c_words.get(words) {
            return a;
        }
        let a = self.cx.new_agg("c struct words".into(), words);
        self.cx.lay.c_words.insert(words.to_vec(), a);
        a
    }

    /// A slot of the aggregate of `words`.
    fn words_slot(&mut self, words: &[Ty]) -> vir::Local {
        let a = self.words_agg(words);
        self.temp(Ty::Agg(a))
    }
}

/// Appends the scalar fields of aggregate `a`, at `base`, nested aggregates flattened.
fn flatten(aggs: &[vir::AggLayout], a: AggId, base: u32, out: &mut Vec<(Ty, u32)>) {
    for &(t, off) in &aggs[a.0 as usize].fields {
        match t {
            Ty::Agg(inner) => flatten(aggs, inner, base + off, out),
            t => out.push((t, base + off)),
        }
    }
}

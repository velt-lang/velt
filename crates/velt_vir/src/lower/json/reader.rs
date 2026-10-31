//! The decoders' pull reader is Velt code: `std/encoding/json/reader.vlt`'s `Reader` (#1668). The glue
//! keeps the runtime reader's operations (`Rt::Json*`, rt_abi_async.md §12.3) as its vocabulary:
//! [`FnLower::reader_rt`] lowers each one to direct calls of the `Reader` methods, with the
//! same results (`u8` status, `u32` token kind, `u64` mark) and the same out-parameters
//! (an owned key or string, an `f64`, `i64`, `u8` or value handle), so the decoders read as
//! before and LLVM inlines the reader into them.

use rustc_hash::FxHashMap;
use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::STR;
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, Cx, FnLower, ScopeKind};
use crate::vir::{BinOp, BlockId, Local, Operand, Place, Proj, Rvalue, Ty};

/// Qualified name of the reader class.
const READER: &str = velt_common::std_item!(encoding_json_reader, "Reader");

impl Cx<'_> {
    /// The `Reader` method (or static function) `name`.
    fn json_reader_fn(&self, name: &str) -> DefId {
        self.json_std_fn(&format!("{READER}.{name}"))
    }

    /// The std function with the qualified name `full` (velt/encoding/json's Velt code).
    pub(in crate::lower) fn json_std_fn(&self, full: &str) -> DefId {
        let hir = self.hir;
        let fns = self.std_fns.get_or_init(|| {
            let mut fns = FxHashMap::default();
            for (i, d) in hir.defs.iter().enumerate() {
                if let hir::Def::Fn(f) = d {
                    if f.name
                        .starts_with(velt_common::std_modules::CANONICAL_PREFIX)
                    {
                        fns.entry(f.name.as_str()).or_insert(DefId(i as u32));
                    }
                }
            }
            fns
        });
        fns.get(full)
            .copied()
            .unwrap_or_else(|| ice(format!("JSON needs std's `{full}`")))
    }

    /// The `Reader` class type.
    pub(super) fn json_reader_ty(&mut self) -> TyId {
        let d = self
            .hir
            .defs
            .iter()
            .position(|d| matches!(d, hir::Def::Adt(a) if a.name == READER))
            .unwrap_or_else(|| ice("JSON.parse needs std/encoding/json/reader.vlt's `Reader`"));
        self.intern(TyKind::Adt(DefId(d as u32), vec![]))
    }
}

impl FnLower<'_, '_> {
    /// `Reader.<name>(pre...)`: a direct call with lowered arguments (the reader first, an
    /// aggregate by address); the result as the method returns it.
    /// An owned result (a string, the reader) is the caller's: glue has no scope to drop it.
    pub(super) fn reader_call(&mut self, name: &str, pre: Vec<Operand>) -> Operand {
        let def = self.cx.json_reader_fn(name);
        self.json_std_call(def, pre)
    }

    /// To `hit` if the reader's last key is `name`, else to `miss`. The key's length is compared
    /// here, with the name's as a constant: most of an object's fields have a name of another
    /// length, and that answer needs no call of `keyIs`.
    pub(super) fn key_is_branch(&mut self, r: Local, name: &str, hit: BlockId, miss: BlockId) {
        let len = self.reader_call("keyLength", vec![Operand::Copy(Place::local(r))]);
        let same = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, len, cint(name.len() as i128, Ty::U64)),
        );
        let compare = self.new_block();
        self.branch(same, compare, miss);
        self.switch_to(compare);
        let lit = self.str_lit(name);
        let la = self.operand_addr(lit, STR);
        let eq = self.reader_call("keyIs", vec![Operand::Copy(Place::local(r)), la]);
        self.branch(eq, hit, miss);
    }

    /// The prelude `JsonValue`'s method `name` (`pre`: the value first, then lowered arguments).
    pub(in crate::lower) fn json_value_call(&mut self, name: &str, pre: Vec<Operand>) -> Operand {
        let def = self
            .cx
            .json_std_fn(&format!("{}.{name}", super::PRELUDE_JSON_VALUE));
        self.json_std_call(def, pre)
    }

    /// A direct call of the std function `def` with lowered arguments.
    fn json_std_call(&mut self, def: DefId, pre: Vec<Operand>) -> Operand {
        let f = self.cx.fn_def(def);
        let (ret, _) = self.cx.call_sig(f);
        // A string the callee takes (`Reader.of` keeps its source) gets a reference of its own:
        // the caller still owns and drops its own.
        let owned: Vec<bool> = f
            .params
            .iter()
            .map(|p| p.mode == hir::PassMode::Owned && matches!(self.cx.kind(p.ty), TyKind::Str))
            .collect();
        let pre = pre
            .into_iter()
            .enumerate()
            .map(|(i, a)| match owned.get(i) {
                Some(true) => {
                    let copy = self.temp(STR);
                    let ca = self.addr(Place::local(copy));
                    self.call_rt(Rt::StrClone, vec![a, ca.clone()], None);
                    ca
                }
                _ => a,
            })
            .collect();
        self.push_scope(ScopeKind::Temps);
        let mut v = self.call_def(def, vec![], pre, &[]);
        if self.cx.needs_drop(ret) {
            v = self.take_owned(v);
        }
        self.pop_scope();
        v
    }

    /// Whether `rt` is one of the reader operations [`Self::reader_rt`] lowers.
    pub(in crate::lower) fn is_reader_rt(rt: Rt) -> bool {
        matches!(
            rt,
            Rt::JsonPeek
                | Rt::JsonObjectStart
                | Rt::JsonArrayStart
                | Rt::JsonNextKey
                | Rt::JsonArrayNext
                | Rt::JsonReadString
                | Rt::JsonReadF64
                | Rt::JsonReadI64
                | Rt::JsonReadBool
                | Rt::JsonReadNull
                | Rt::JsonSkipValue
                | Rt::JsonSkipLookahead
                | Rt::JsonSkipUnknown
                | Rt::JsonMark
                | Rt::JsonReset
                | Rt::JsonEnd
                | Rt::JsonNullAbsent
                | Rt::JsonError
        )
    }

    /// The reader operation `rt` with the runtime reader's arguments and results (`args[0]` is
    /// the reader), as calls of the Velt `Reader`'s methods.
    pub(super) fn reader_rt(&mut self, rt: Rt, args: Vec<Operand>, dest: Option<Place>) {
        let r = args[0].clone();
        let status = |lw: &mut Self, name: &str| {
            let ok = lw.reader_call(name, vec![r.clone()]);
            lw.rvalue_temp(Ty::U8, Rvalue::Cast(ok, Ty::U8))
        };
        let result = match rt {
            Rt::JsonPeek => self.reader_call("peek", vec![r.clone()]),
            Rt::JsonObjectStart => status(self, "openObject"),
            Rt::JsonArrayStart => status(self, "openArray"),
            Rt::JsonArrayNext => self.reader_call("arrayNext", vec![r.clone()]),
            Rt::JsonReadNull => status(self, "readNull"),
            Rt::JsonSkipValue => status(self, "skipValue"),
            Rt::JsonSkipLookahead => status(self, "skipLookahead"),
            Rt::JsonSkipUnknown => status(self, "skipUnknown"),
            Rt::JsonEnd => status(self, "finish"),
            Rt::JsonNullAbsent => status(self, "nullAbsent"),
            Rt::JsonMark => self.reader_call("mark", vec![r.clone()]),
            Rt::JsonReset => {
                self.reader_call("reset", vec![r.clone(), args[1].clone()]);
                return;
            }
            Rt::JsonNextKey => {
                // 1: the key goes to `*args[1]` (owned).
                let step = self.reader_call("nextKey", vec![r.clone()]);
                let more = self.rvalue_temp(
                    Ty::Bool,
                    Rvalue::Binary(BinOp::Eq, step.clone(), cint(1, Ty::U8)),
                );
                self.text_out_if(more, &r, &args[1], "key");
                step
            }
            Rt::JsonReadString => {
                let ok = self.reader_call("readString", vec![r.clone()]);
                self.text_out_if(ok.clone(), &r, &args[1], "value");
                self.rvalue_temp(Ty::U8, Rvalue::Cast(ok, Ty::U8))
            }
            Rt::JsonReadF64 => self.read_out(&r, "readF64", "lastF64", &args[1], Ty::F64),
            Rt::JsonReadI64 => self.read_out(&r, "readI64", "lastI64", &args[1], Ty::I64),
            Rt::JsonReadBool => self.read_out(&r, "readBool", "lastI64", &args[1], Ty::U8),
            Rt::JsonError => {
                // `*args[3] = r.message(*args[1], *args[2])`.
                let m =
                    self.reader_call("message", vec![r.clone(), args[1].clone(), args[2].clone()]);
                let out = Place {
                    local: self.copy_to_temp(args[3].clone(), Ty::Ptr),
                    proj: vec![Proj::Deref(STR)],
                };
                self.assign(out, Rvalue::Use(m));
                return;
            }
            _ => ice("not a JSON reader operation"),
        };
        if let Some(d) = dest {
            self.assign(d, Rvalue::Use(result));
        }
    }

    /// If `cond`: `*out = r.<method>()` (an owned string at the address `out`).
    fn text_out_if(&mut self, cond: Operand, r: &Operand, out: &Operand, method: &str) {
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(cond, yes, join);
        self.switch_to(yes);
        let key = self.reader_call(method, vec![r.clone()]);
        let p = Place {
            local: self.copy_to_temp(out.clone(), Ty::Ptr),
            proj: vec![Proj::Deref(STR)],
        };
        self.assign(p, Rvalue::Use(key));
        self.goto(join);
        self.switch_to(join);
    }

    /// `r.<read>()`, and then `*out = r.<last>()` as `ty` (a scalar out-parameter, written
    /// whatever the result: a failed decode discards it). The status as a `u8`.
    pub(super) fn read_out(
        &mut self,
        r: &Operand,
        read: &str,
        last: &str,
        out: &Operand,
        ty: Ty,
    ) -> Operand {
        let ok = self.reader_call(read, vec![r.clone()]);
        let v = self.reader_call(last, vec![r.clone()]);
        let v = match ty {
            Ty::U8 => self.rvalue_temp(Ty::U8, Rvalue::Cast(v, Ty::U8)),
            _ => v,
        };
        let p = Place {
            local: self.copy_to_temp(out.clone(), Ty::Ptr),
            proj: vec![Proj::Deref(ty)],
        };
        self.assign(p, Rvalue::Use(v));
        self.rvalue_temp(Ty::U8, Rvalue::Cast(ok, Ty::U8))
    }
}

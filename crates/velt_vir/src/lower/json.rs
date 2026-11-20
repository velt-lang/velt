//! What lowering still knows of JSON and records (the decoders and writers are std's, written by
//! source generators: velt/encoding/json/generate): the prelude's `JsonValue` (printed from the
//! runtime tree its `[JSON_RUNTIME_HANDLE]()` makes, glue/format.rs) and `JsonError` (an uncaught
//! one, entry.rs), and the key order of a `Record`'s entries (glue/format_map.rs).

use rustc_hash::FxHashMap;
use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, Cx, FnLower, ScopeKind};
use crate::vir::{self, BinOp, Local, Operand, Place, Proj, Rvalue, Ty, STR_AGG};

const STR: Ty = Ty::Agg(STR_AGG);

/// Qualified name of the prelude's dynamic JSON value class.
const PRELUDE_JSON_VALUE: &str = velt_common::std_item!(prelude_json_value, "JsonValue");

/// The name of the prelude's `JsonError` class.
const PRELUDE_JSON_ERROR: &str = velt_common::std_item!(prelude_json_error, "JsonError");

impl Cx<'_> {
    /// The prelude's `JsonError` definition: `std/prelude/json/json_error::JsonError`, else (a program
    /// built without the real prelude, as in unit tests) the first type named `JsonError`. A
    /// user's own class of that name is never it.
    pub(super) fn json_error_def(&self) -> Option<hir::DefId> {
        let named = |f: &dyn Fn(&str) -> bool| {
            self.hir
                .defs
                .iter()
                .position(|d| matches!(d, hir::Def::Adt(a) if f(&a.name)))
        };
        named(&|n| n == PRELUDE_JSON_ERROR)
            .or_else(|| {
                if self
                    .hir
                    .defs
                    .iter()
                    .any(|d| matches!(d, hir::Def::Adt(a) if a.name.starts_with(velt_common::std_dir!(prelude))))
                {
                    return None;
                }
                named(&|n| {
                    n == "JsonError" || n.ends_with("::JsonError") || n.ends_with(".JsonError")
                })
            })
            .map(|d| hir::DefId(d as u32))
    }

    /// Is `t` the prelude's `JsonValue` type (written by its `[JSON_WRITE]`, decoded by its
    /// `[JSON_FROM_READER]`)? Matched by its exact qualified name, so a user class named `JsonValue`
    /// (even in a module named `json`) stays an ordinary class.
    pub(in crate::lower) fn is_json_value(&self, t: TyId) -> bool {
        match self.raw_kind(t) {
            TyKind::Adt(d, _) => match self.hir.def(*d) {
                hir::Def::Adt(a) => a.name == PRELUDE_JSON_VALUE,
                _ => false,
            },
            _ => false,
        }
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
}

impl FnLower<'_, '_> {
    /// The prelude `JsonValue`'s method `name` (`pre`: the value first, then lowered arguments).
    pub(in crate::lower) fn json_value_call(&mut self, name: &str, pre: Vec<Operand>) -> Operand {
        let def = self
            .cx
            .json_std_fn(&format!("{}.{name}", PRELUDE_JSON_VALUE));
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

    /// The method `name` of class `class` (a prelude class whose methods the glue calls).
    pub(in crate::lower) fn class_method(&mut self, class: DefId, name: &str) -> DefId {
        let owner = self.cx.adt_def(class).name.clone();
        let full = format!("{owner}.{name}");
        let found = self.cx.hir.defs.iter().position(|d| match d {
            hir::Def::Fn(f) => f.name == full && f.self_ty.is_some(),
            _ => false,
        });
        let i = found.unwrap_or_else(|| ice(format_args!("no method `{full}`")));
        DefId(i as u32)
    }

    /// The order in which to walk the entries of the record at `place` (of the prelude `Record`
    /// type `ty`): its `[RECORD_POSITIONS]()`, the entry positions in JavaScript's key order, or empty
    /// for insertion order (always, unless a key that may be an array index was added).
    pub(in crate::lower) fn record_order(&mut self, place: &Place, ty: TyId) -> RecordOrder {
        let TyKind::Adt(d, targs) = self.cx.kind(ty) else {
            ice("Record method on a non-ADT type")
        };
        let def = self.class_method(d, velt_common::std_key!(record_positions));
        let ret = match self.cx.hir.def(def) {
            hir::Def::Fn(f) => f.ret,
            _ => ice("Record[RECORD_POSITIONS] is not a function"),
        };
        let f = self.cx.func_for_direct(def, targs.clone(), self.mode);
        let arr_ty = self.cx.subst(ret, &targs);
        let TyKind::Array(elem) = self.cx.kind(arr_ty) else {
            ice("Record[RECORD_POSITIONS] does not return an array")
        };
        let vt = self.cx.ty(arr_ty);
        let arr = self.temp(vt);
        let mut args = vec![Operand::Copy(place.clone())];
        // The array is an aggregate or a box, as the program uses arrays; an aggregate comes back
        // through an out pointer.
        let dest = match self.cx.ret_abi(arr_ty, None).out {
            Some(_) => {
                let a = self.addr(Place::local(arr));
                args.push(a);
                None
            }
            None => Some(Place::local(arr)),
        };
        self.call(vir::Callee::Func(f), args, dest, false);
        let content = self.content(&Place::local(arr), arr_ty);
        RecordOrder {
            arr,
            arr_ty,
            elem,
            content,
        }
    }

    /// Release the array of `record_order`.
    pub(in crate::lower) fn drop_record_order(&mut self, order: RecordOrder) {
        self.drop_glue(Place::local(order.arr), order.arr_ty);
    }

    /// How many entry positions a loop over a map with `len` positions walks in `order`: the
    /// order's length, or `len` when it is empty.
    pub(in crate::lower) fn order_len(
        &mut self,
        order: Option<&RecordOrder>,
        len: Operand,
    ) -> Operand {
        let Some(o) = order else {
            return len;
        };
        let n = Operand::Copy(proj(&o.content, Proj::Field(1)));
        let out = self.temp(Ty::U64);
        self.assign(Place::local(out), Rvalue::Use(len));
        let some = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, n.clone(), cint(0, Ty::U64)),
        );
        let (set, join) = (self.new_block(), self.new_block());
        self.branch(some, set, join);
        self.switch_to(set);
        self.assign(Place::local(out), Rvalue::Use(n));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(out))
    }

    /// The entry position of step `i` of such a loop: `order[i]`, or `i` when it is empty.
    pub(in crate::lower) fn order_pos(
        &mut self,
        order: Option<&RecordOrder>,
        i: Operand,
    ) -> Operand {
        let Some(o) = order else {
            return i;
        };
        let n = Operand::Copy(proj(&o.content, Proj::Field(1)));
        let out = self.temp(Ty::U64);
        self.assign(Place::local(out), Rvalue::Use(i.clone()));
        let some = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, n, cint(0, Ty::U64)));
        let (set, join) = (self.new_block(), self.new_block());
        self.branch(some, set, join);
        self.switch_to(set);
        let at = self.elem_place(&o.content, i, o.elem);
        self.assign(Place::local(out), Rvalue::Use(Operand::Copy(at)));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(out))
    }
}

/// The order of a record's entries for a loop over its map (`FnLower::record_order`).
pub(in crate::lower) struct RecordOrder {
    /// The `usize[]` of `[RECORD_POSITIONS]()`, owned by the loop's caller.
    arr: Local,
    arr_ty: TyId,
    elem: TyId,
    /// Its inline value.
    content: Place,
}

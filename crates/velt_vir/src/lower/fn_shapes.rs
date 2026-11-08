//! `length` and `name` of function values (hir_encodings.md "Function values"): a function value
//! is `{ code, env }` and says nothing else, and a function of one type may be any function of
//! that type. So a read goes by the code, through two glues built last (`Cx::build_fn_tables`),
//! when every function that became a value has its code: a named function's or a closure
//! without captures' thunk, or the code of a closure with captures. Each compares the code with
//! those whose answer is not the default (`0`, `""`). Only a program that reads either, or
//! holds a function in an `any`, has them; nothing is added to a function value.

use velt_sema::hir::{self, DefId, FloatTy, TyId, TyKind};

use super::any_member::ST_OK;
use super::glue::Glue;
use super::kinds::ThunkKind;
use super::operand::proj;
use super::rt::Rt;
use super::{cfunc, cint, Cx, FnLower, Work};
use crate::vir::{self, BinOp, Callee, FuncId, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// What `console.log` calls a function, by its kind: plain, `async`, a generator, both.
const KINDS: [&str; 4] = [
    "Function",
    "AsyncFunction",
    "GeneratorFunction",
    "AsyncGeneratorFunction",
];

/// The function whose value has the code of `work`, if `work` is such code.
fn value_def(work: &Work) -> Option<(DefId, bool)> {
    match work {
        Work::Checked(w) | Work::Read(w) | Work::ReadOwn(w) | Work::Deferred(w) => value_def(w),
        Work::Thunk(ThunkKind::Env(_), d, _) => Some((*d, true)),
        Work::Fn(d, _) | Work::Frozen(d, _, _, _, _) => Some((*d, false)),
        _ => None,
    }
}

impl Cx<'_> {
    /// The glue `g` (`FnLength` or `FnName`), built last.
    fn fn_table(&mut self, g: Glue) -> FuncId {
        let unit = self.intern(TyKind::Unit);
        let work = Work::Glue(g, unit);
        if let Some(&f) = self.work_map.get(&work) {
            return f;
        }
        let f = FuncId(self.funcs.len() as u32);
        self.funcs.push(None);
        self.work_map.insert(work.clone(), f);
        self.fn_tables.push((f, work));
        f
    }

    /// Build the glues of [`Self::fn_table`]; everything else is built by now.
    pub(super) fn build_fn_tables(&mut self) {
        for (fid, work) in std::mem::take(&mut self.fn_tables) {
            self.build_now(fid, &work);
        }
    }

    /// The code of each function that may be a value and its `(name, length, kind)`, in a fixed
    /// order; the kind is what `console.log` writes (`KINDS`).
    fn fn_codes(&self) -> Vec<(FuncId, String, u32, u8)> {
        let mut out = vec![];
        for (work, &fid) in &self.work_map {
            let Some((d, thunk)) = value_def(work) else {
                continue;
            };
            let hir::Def::Fn(value) = self.hir.def(d) else {
                continue;
            };
            let mut f = value;
            // Without a thunk, only a closure with captures has its code as a value.
            if !thunk && f.captures.is_empty() {
                continue;
            }
            let mut shape = self.hir.fn_shapes.get(&f.span);
            // A wrapper's own name (`bound f`) goes before the wrapped function's.
            let mut name = shape.and_then(|s| s.name.as_deref());
            while let Some(w) = shape.and_then(|s| s.wraps) {
                let hir::Def::Fn(inner) = self.hir.def(w) else {
                    break;
                };
                f = inner;
                shape = self.hir.fn_shapes.get(&f.span);
                name = name.or(shape.and_then(|s| s.name.as_deref()));
            }
            let name = name.unwrap_or(hir::declared_fn_name(&f.name));
            let kind = u8::from(f.is_async) + 2 * u8::from(f.is_generator);
            out.push((fid, name.to_string(), shape.map_or(0, |s| s.length), kind));
        }
        out.sort_by_key(|e| e.0 .0);
        out
    }
}

impl FnLower<'_, '_> {
    /// `f.length` (`Intrinsic::FnLength`).
    pub(super) fn fn_length(&mut self, f: &hir::Expr) -> Operand {
        let code = self.fn_code_of(f);
        self.fn_length_of(code)
    }

    /// `f.name` (`Intrinsic::FnName`).
    pub(super) fn fn_name(&mut self, f: &hir::Expr) -> Operand {
        let code = self.fn_code_of(f);
        self.fn_name_of(code)
    }

    fn fn_code_of(&mut self, f: &hir::Expr) -> Operand {
        let fty = self.sub(f.ty);
        let fv = self.expr(f);
        let fp = self.place_of(fv, fty);
        Operand::Copy(proj(&fp, Proj::Field(0)))
    }

    fn fn_length_of(&mut self, code: Operand) -> Operand {
        let glue = self.cx.fn_table(Glue::FnLength);
        let out = Place::local(self.temp(Ty::F64));
        self.call(Callee::Func(glue), vec![code], Some(out.clone()), false);
        Operand::Copy(out)
    }

    fn fn_name_of(&mut self, code: Operand) -> Operand {
        let glue = self.cx.fn_table(Glue::FnName);
        let st = self.cx.str_ty();
        let vt = self.cx.ty(st);
        let out = Place::local(self.temp(vt));
        let at = self.addr(out.clone());
        self.call(Callee::Func(glue), vec![code, at], None, false);
        Operand::Copy(out)
    }

    /// A function value in the text of `console.log`, as Node writes it (`format.rs`).
    pub(super) fn format_fn(&mut self, buf: &Operand, place: &Place) {
        let code = Operand::Copy(proj(place, Proj::Field(0)));
        let kind = self.cx.fn_table(Glue::FnKind);
        let k = Place::local(self.temp(Ty::U8));
        self.call(
            Callee::Func(kind),
            vec![code.clone()],
            Some(k.clone()),
            false,
        );
        let join = self.new_block();
        for (i, word) in KINDS.iter().enumerate() {
            let is = Rvalue::Binary(BinOp::Eq, Operand::Copy(k.clone()), cint(i as i128, Ty::U8));
            let is = self.rvalue_temp(Ty::Bool, is);
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(is, yes, no);
            self.switch_to(yes);
            self.push_text(buf, &format!("[{word}"));
            self.goto(join);
            self.switch_to(no);
        }
        self.goto(join);
        self.switch_to(join);
        let name = self.fn_name_of(code);
        let st = self.cx.str_ty();
        let vt = self.cx.ty(st);
        let name = Place::local(self.copy_to_temp(name, vt));
        // The word of a string's lengths: zero for an empty one.
        let lengths = Operand::Copy(proj(&name, Proj::Field(1)));
        let none = Rvalue::Binary(BinOp::Eq, lengths, cint(0, Ty::U64));
        let none = self.rvalue_temp(Ty::Bool, none);
        let (anonymous, named, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(none, anonymous, named);
        self.switch_to(anonymous);
        self.push_text(buf, " (anonymous)]");
        self.goto(done);
        self.switch_to(named);
        self.push_text(buf, ": ");
        let at = self.addr(name);
        self.call_rt(Rt::StrbufPushStr, vec![buf.clone(), at], None);
        self.push_text(buf, "]");
        self.goto(done);
        self.switch_to(done);
    }

    /// The body of `Glue::FnKind`: the index in `KINDS` of the function with code `code`.
    pub(super) fn fn_kind_body(&mut self, code: vir::Local) {
        for (f, _, _, kind) in self.cx.fn_codes() {
            if kind != 0 {
                let next = self.on_code(code, f);
                self.terminate(Terminator::Return(cint(i128::from(kind), Ty::U8)));
                self.switch_to(next);
            }
        }
        self.terminate(Terminator::Return(cint(0, Ty::U8)));
    }

    /// `length` and `name` in the `AnyGet` slot body of a function type: answered here, and any
    /// other key goes on in the current block.
    pub(super) fn fn_own_get(&mut self, data: vir::Local, key: vir::Local, out: &Place, ty: TyId) {
        // A class object: one closure type for every class, whose code says nothing of the
        // class, so `name` and `length` stop as before (#924).
        if self.class_object_tag(ty).is_some() {
            return;
        }
        let vp = self.any_value_place(data, ty);
        let next = self.on_key(key, "length");
        let n = self.fn_length_of(Operand::Copy(proj(&vp, Proj::Field(0))));
        let f64_ = self.cx.intern(TyKind::Float(FloatTy::F64));
        self.any_box_into(n, f64_, out);
        self.ret_status(ST_OK);
        self.switch_to(next);
        let next = self.on_key(key, "name");
        let s = self.fn_name_of(Operand::Copy(proj(&vp, Proj::Field(0))));
        let st = self.cx.str_ty();
        self.any_box_into(s, st, out);
        self.ret_status(ST_OK);
        self.switch_to(next);
    }

    /// The block for code `code` equal to `f`'s (the current block when it is), the other
    /// returned.
    fn on_code(&mut self, code: vir::Local, f: FuncId) -> vir::BlockId {
        let same = Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(code)), cfunc(f));
        let same = self.rvalue_temp(Ty::Bool, same);
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(same, yes, no);
        self.switch_to(yes);
        no
    }

    /// The body of `Glue::FnLength`.
    pub(super) fn fn_length_body(&mut self, code: vir::Local) {
        for (f, _, length, _) in self.cx.fn_codes() {
            if length != 0 {
                let next = self.on_code(code, f);
                let v = Operand::Const(vir::Const::Float(f64::from(length)), Ty::F64);
                self.terminate(Terminator::Return(v));
                self.switch_to(next);
            }
        }
        let v = Operand::Const(vir::Const::Float(0.0), Ty::F64);
        self.terminate(Terminator::Return(v));
    }

    /// The body of `Glue::FnName`.
    pub(super) fn fn_name_body(&mut self, code: vir::Local, out: vir::Local) {
        let st = self.cx.str_ty();
        let vt = self.cx.ty(st);
        let at = Place {
            local: out,
            proj: vec![Proj::Deref(vt)],
        };
        for (f, name, _, _) in self.cx.fn_codes() {
            if !name.is_empty() {
                let next = self.on_code(code, f);
                let s = self.str_lit(&name);
                self.store(at.clone(), s);
                self.terminate(Terminator::Return(super::unit()));
                self.switch_to(next);
            }
        }
        let s = self.str_lit("");
        self.store(at, s);
        self.terminate(Terminator::Return(super::unit()));
    }
}

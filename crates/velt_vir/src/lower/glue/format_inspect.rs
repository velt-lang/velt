//! What `console.log` prints for a class object that has an `[INSPECT]()` method, as Node prints
//! a custom inspect: a string result as it is, std's `NativeInspect` and `NamedInspect` texts,
//! an array after the name and its length, and any other value after a name: the object's own
//! class, or for the classes of [`FIXED_NAMES`] the std class's.

use velt_sema::hir::{DefId, TyId, TyKind};

use crate::lower::hook_calls::{self as hooks, ty, HookTy};
use crate::lower::operand::proj;
use crate::lower::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// The std classes Node prints under a fixed name, whatever the object's class: undici's and
/// Node's own inspects write `Headers`, `Request`, `Response` and `Blob` themselves, where the
/// other classes' write `this.constructor.name`.
const FIXED_NAMES: [&str; 4] = [
    concat!(velt_common::std_dir!(net_http_fetch), "headers::Headers"),
    concat!(velt_common::std_dir!(net_http_fetch), "request::Request"),
    concat!(velt_common::std_dir!(net_http_fetch), "response::Response"),
    concat!(velt_common::std_dir!(net_http_fetch), "blob::Blob"),
];

impl FnLower<'_, '_> {
    /// Print the class object `obj` through its `[INSPECT]()` method `m`, as Node prints a custom
    /// inspect: a string result raw, any other value after the class name, at the object's depth
    /// (`Headers { a: '1' }`).
    pub(super) fn format_inspect(
        &mut self,
        buf: &Operand,
        obj: Operand,
        ty: TyId,
        m: DefId,
        depth: &Operand,
    ) {
        // The method is user code: what it prints is numbered on its own (std's `inspectSave`),
        // with custom inspect pieces of its own (`inspectAtomsSave`).
        let saved = hooks::inspect_save(self).map(|save| save.emit(self).op());
        let atoms = hooks::inspect_atoms_save(self).map(|save| save.emit(self).op());
        let (res, rty) = self.call_hook(m, obj, ty, None);
        if let Some(restore) =
            atoms.and_then(|s| hooks::inspect_atoms_restore(self, ty::U64::arg(s)))
        {
            _ = restore.emit(self);
        }
        if let Some(restore) = saved.and_then(|s| hooks::inspect_restore(self, ty::U64::arg(s))) {
            _ = restore.emit(self);
        }
        let name = self.cx.type_name(ty);
        if matches!(self.cx.kind(rty), TyKind::Str) {
            self.push_inspect_text(buf, &res);
        } else if self.is_named_inspect(rty) {
            // What Node prints under `this.constructor.name` (`URL`, `URLSearchParams`): the
            // object's class, a subclass's too, then the text; past the depth limit the name and
            // the struct's `cut` (`URL {}`; a subclass `U [URL] {}`, by the base's
            // `Symbol.toStringTag`), or, when that is empty, `[Object]`.
            let text = self.field_place(&res, rty, 0);
            let cut = self.field_place(&res, rty, 1);
            let cut_name = match self.std_inspect_owner(m) {
                Some(base) if base != name => format!("{name} [{base}]"),
                _ => name.clone(),
            };
            let cut_text = |lw: &mut Self| {
                let n = lw.str_len(Operand::Copy(cut.clone()));
                let given =
                    lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, n, cint(0, Ty::U64)));
                let (given_bb, plain_bb, done) = (lw.new_block(), lw.new_block(), lw.new_block());
                lw.branch(given, given_bb, plain_bb);
                lw.switch_to(given_bb);
                lw.push_text(buf, &cut_name);
                let a = lw.addr(cut.clone());
                lw.push_str(buf, a);
                lw.goto(done);
                lw.switch_to(plain_bb);
                lw.push_text(buf, "[Object]");
                lw.goto(done);
                lw.switch_to(done);
            };
            self.within_depth_by(buf, depth, cut_text, None, |lw, _| {
                // The name and the text are one piece, like a custom inspect's string.
                let start = lw.builder_len(buf);
                lw.push_text(buf, &name);
                let a = lw.addr(text.clone());
                lw.push_str(buf, a);
                lw.inspect_atom(buf, start);
            });
        } else if self.is_native_inspect(rty) {
            // What Node prints for one of its own objects (`ArrayBuffer`, `URL`): the text, cut
            // past the depth limit to the struct's `cut` (`URL {}`), or, when that is empty, to
            // `[ArrayBuffer]`, as Node cuts its objects.
            let text = self.field_place(&res, rty, 0);
            let cut = self.field_place(&res, rty, 1);
            let cut_text = |lw: &mut Self| {
                let n = lw.str_len(Operand::Copy(cut.clone()));
                let given =
                    lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, n, cint(0, Ty::U64)));
                let (given_bb, named_bb, done) = (lw.new_block(), lw.new_block(), lw.new_block());
                lw.branch(given, given_bb, named_bb);
                lw.switch_to(given_bb);
                let a = lw.addr(cut.clone());
                lw.push_str(buf, a);
                lw.goto(done);
                lw.switch_to(named_bb);
                lw.push_text(buf, &format!("[{name}]"));
                lw.goto(done);
                lw.switch_to(done);
            };
            self.within_depth_by(buf, depth, cut_text, None, |lw, _| {
                lw.push_inspect_text(buf, &text);
            });
        } else if let TyKind::Array(_) = self.cx.kind(rty) {
            // An array: after the name and its length, as Node prints an array subclass or a
            // typed array (`Uint8Array(3) [ 1, 2, 3 ]`), cut to `[Uint8Array]` past the depth
            // limit.
            self.within_depth(buf, depth, &format!("[{name}]"), None, |lw, _| {
                let start = lw.builder_len(buf);
                let arr = lw.content(&res, rty);
                let len = Operand::Copy(proj(&arr, Proj::Field(1)));
                lw.push_text(buf, &format!("{name}("));
                let u64_ = lw.cx.intern(TyKind::Int(velt_sema::hir::IntTy::U64));
                lw.push_scalar(buf, len, u64_);
                lw.push_text(buf, ") ");
                lw.inspect_atom(buf, start);
                lw.format_nested(buf, &res, rty, depth);
            });
        } else {
            // The string, or the name before the value, is kept as one piece when the value is
            // broken across lines: node inserts a custom inspect's text as it is.
            let start = self.builder_len(buf);
            let name = self.fixed_inspect_name(ty, m).unwrap_or(name);
            self.push_text(buf, &format!("{name} "));
            self.inspect_atom(buf, start);
            self.format_nested(buf, &res, rty, depth);
        }
        self.drop_glue(res, rty);
    }

    /// The name of the std class that declares `[INSPECT]()` method `m`; `None` for a program's.
    fn std_inspect_owner(&self, m: DefId) -> Option<String> {
        let owner = self.cx.fn_def(m).self_ty?;
        let name = &self.cx.adt_def(self.class_def(owner)?).name;
        velt_common::std_modules::is_std(name).then(|| self.cx.type_name(owner))
    }

    /// The name a `ty` object is printed under when its `[INSPECT]()` `m` is one of
    /// [`FIXED_NAMES`]' classes': std's class the object is or extends, not a program's subclass
    /// (`Headers { … }` for a `class H extends Headers`, `File { … }` for a `File`, which has
    /// `Blob`'s). `None` for every other class, which prints under its own name, as Node writes
    /// `this.constructor.name` for an `EventTarget`, an `Event` or an `AbortController`.
    fn fixed_inspect_name(&mut self, ty: TyId, m: DefId) -> Option<String> {
        let owner = self.class_def(self.cx.fn_def(m).self_ty?)?;
        if !FIXED_NAMES.contains(&self.cx.adt_def(owner).name.as_str()) {
            return None;
        }
        let mut class = ty;
        for _ in 0..16 {
            let TyKind::Adt(d, args) = self.cx.kind(class) else {
                return None;
            };
            let def = self.cx.adt_def(d);
            if velt_common::std_modules::is_std(&def.name) {
                return Some(self.cx.type_name(class));
            }
            class = self.cx.subst(def.base?, &args);
        }
        None
    }

    /// The definition of class type `ty`.
    fn class_def(&self, ty: TyId) -> Option<DefId> {
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) => Some(d),
            _ => None,
        }
    }

    /// The string at `text` (a custom inspect's), kept as one piece when the value around it is
    /// broken across lines: node inserts a custom inspect's text as it is.
    fn push_inspect_text(&mut self, buf: &Operand, text: &Place) {
        let start = self.builder_len(buf);
        let a = self.addr(text.clone());
        self.push_str(buf, a);
        self.inspect_atom(buf, start);
    }

    /// Is `ty` std's `NativeInspect` (std/lang/typed_array/native_inspect.velt), the result of an
    /// `[INSPECT]()` that prints one of Node's own objects?
    fn is_native_inspect(&self, ty: TyId) -> bool {
        const NAME: &str = velt_common::std_item!(lang_typed_array_native_inspect, "NativeInspect");
        matches!(self.cx.kind(ty), TyKind::Adt(d, _) if self.cx.adt_def(d).name == NAME)
    }

    /// Is `ty` std's `NamedInspect` (std/lang/typed_array/named_inspect.velt), the result of an
    /// `[INSPECT]()` whose text follows the name of the object's class?
    fn is_named_inspect(&self, ty: TyId) -> bool {
        const NAME: &str = velt_common::std_item!(lang_typed_array_named_inspect, "NamedInspect");
        matches!(self.cx.kind(ty), TyKind::Adt(d, _) if self.cx.adt_def(d).name == NAME)
    }
}

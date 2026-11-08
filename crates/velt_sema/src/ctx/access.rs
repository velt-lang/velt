//! Diagnostics and the definition tables' accessors of the checking context (moved out of
//! `ctx/mod.rs`, unchanged).

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::{AdtInfo, DefInfo, EnumInfo, FnInfo, GlobalInfo, IfaceInfo};
use crate::hir::{DefId, TyId};

impl<'m> Ctx<'m> {
    pub fn error(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    pub fn err(&mut self, msg: impl Into<String>, span: Span) {
        self.diags.push(Diagnostic::error(msg, span));
    }

    /// `msg` for a name `name` that nothing declares: with a note naming the import when it is
    /// one of the SIMD types, which are not global (`velt/simd`).
    pub fn not_found(&mut self, msg: String, name: &str, span: Span) {
        if !self.poisoned.is_empty() && self.poisoned.contains(&(span.file, name.to_string())) {
            return;
        }
        let note = crate::not_found_notes::note(&msg, name);
        let d = Diagnostic::error(msg, span);
        self.diags.push(match note {
            Some(n) => d.with_note(n),
            None => d,
        });
    }

    /// Define the closure of a rolled-back check again as `info` (`body::recheck`).
    pub(crate) fn redefine_closure(&mut self, id: DefId, span: Span, info: DefInfo<'m>) {
        self.fn_defs.push(id);
        self.defs[id.0 as usize] = None;
        self.info[id.0 as usize] = info;
        self.def_spans[id.0 as usize] = span;
    }

    pub fn alloc_def(&mut self, span: Span, info: DefInfo<'m>) -> DefId {
        let id = DefId(self.defs.len() as u32);
        if matches!(info, DefInfo::Fn(_)) {
            self.fn_defs.push(id);
        }
        self.defs.push(None);
        self.info.push(info);
        self.def_spans.push(span);
        id
    }

    pub fn fn_info(&self, d: DefId) -> &FnInfo<'m> {
        match &self.info[d.0 as usize] {
            DefInfo::Fn(f) => f,
            _ => panic!("ICE: def {d:?} is not a function"),
        }
    }

    pub fn fn_info_mut(&mut self, d: DefId) -> &mut FnInfo<'m> {
        match &mut self.info[d.0 as usize] {
            DefInfo::Fn(f) => f,
            _ => panic!("ICE: def {d:?} is not a function"),
        }
    }

    pub fn try_fn(&self, d: DefId) -> Option<&FnInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Fn(f) => Some(f),
            _ => None,
        }
    }

    pub fn adt(&self, d: DefId) -> Option<&AdtInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Adt(a) => Some(a),
            _ => None,
        }
    }

    pub fn adt_mut(&mut self, d: DefId) -> &mut AdtInfo<'m> {
        match &mut self.info[d.0 as usize] {
            DefInfo::Adt(a) => a,
            _ => panic!("ICE: def {d:?} is not an ADT"),
        }
    }

    pub fn enum_info(&self, d: DefId) -> Option<&EnumInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Enum(e) => Some(e),
            _ => None,
        }
    }

    pub fn iface(&self, d: DefId) -> Option<&IfaceInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Iface(i) => Some(i),
            _ => None,
        }
    }

    pub fn global(&self, d: DefId) -> Option<&GlobalInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Global(g) => Some(g),
            _ => None,
        }
    }

    /// A string or `string | null`: a value whose copy is a refcount increment at most, so moves
    /// of it are soft (`crate::ownership::strings`).
    pub fn is_string_value(&self, t: TyId) -> bool {
        t == self.ty.str_ || self.ty.opt_payload(t) == Some(self.ty.str_)
    }

    /// Semantics stage 2: a non-Copy value that is *shared* — another reference to the same
    /// value — where its place stays in use or cannot be moved from, instead of being moved
    /// (strings, objects, arrays, maps, closures, promises: another holder of the same
    /// promise, #212): every type that is not Copy.
    pub fn is_shared_value(&mut self, t: TyId) -> bool {
        if let Some(&b) = self.shared_memo.get(&t) {
            return b;
        }
        let b = !self.is_copy(t);
        self.shared_memo.insert(t, b);
        b
    }

    /// Fully qualified name of a module-level item.
    pub fn qualify(&self, module: usize, name: &str) -> String {
        if module == self.root {
            name.to_string()
        } else {
            format!("{}::{}", self.modules[module].path, name)
        }
    }
}

//! What calls of `declare c` and host functions share (c_call.rs, host.rs): their buffer
//! arguments (an array's data pointer and length, or a view's: system_views.rs), kept alive across
//! the arguments after them; `errno` captured into the slot `lastOsError()` reads; and the errors
//! for calls the target can't make, reported once lowering is done.

use velt_sema::hir::{self, DefId, TyId};

use super::sequence::Later;
use super::{ice, FnLower};
use crate::c_abi::{self, ErrnoSource};
use crate::tls::TlsCall;
use crate::vir::{self, Operand, Place, Proj, Rvalue, Ty};

/// An argument of a C or host function, lowered.
pub(super) enum Arg {
    Scalar(Operand),
    /// The place of the array's or struct's inline value, and its type.
    Value(Place, TyId),
    /// A typed array or `DataView` (a class extending std's `BufferView`): the place of the
    /// pointer to it, its class type, and the message when its bytes are outside its buffer;
    /// the buffer's header is read at the call.
    View {
        place: Place,
        t: TyId,
        outside: String,
    },
    /// `xs.subarray(begin, end)` of a `u8[]`: the place of the array's value and its type, and
    /// the two positions evaluated (with their types); the header is read at the call.
    Sub {
        place: Place,
        t: TyId,
        begin: (Operand, TyId),
        end: (Operand, TyId),
    },
}

impl FnLower<'_, '_> {
    /// Before the buffer `e` is evaluated: when a later argument may change memory, record the
    /// path to its value as [`Self::expr_held`] does, so an array a later argument can free
    /// through a module `let` or a closure's cell (`g = Buffer.alloc(1)`) has a counted type,
    /// and [`Self::hold_buffer`] keeps the array the argument named (JS reads it first).
    pub(super) fn note_held_buffer(&mut self, e: &hir::Expr, later: Later) {
        if !later.memory || self.dead() {
            return;
        }
        let ty = self.sub(e.ty);
        if self.cx.needs_drop(ty) && !self.through_counted(e, ty) {
            self.in_shared_cell(e);
        }
    }

    /// Keep the buffer value `v` (of type `ty`) alive across the arguments after it: when one of
    /// them may change memory (`later.memory`) it can free what `v` points to through another
    /// reference (a module `let` it reassigns), so `v` is retained for the statement and released
    /// after the call. A value nothing else reaches, or no later argument can free, is untouched.
    pub(super) fn hold_buffer(
        &mut self,
        v: Operand,
        ty: velt_sema::hir::TyId,
        later: Later,
    ) -> Operand {
        let cty = self.sub(ty);
        if !later.memory || !self.cx.counted(cty) {
            return v;
        }
        let shared = self.share_value(v, cty);
        self.own_value(shared, cty)
    }

    /// Evaluate a C or host function's argument for its side effects only, on a target that lacks
    /// the function (the call is a compile error, reported once lowering is done). A view argument is
    /// `Intrinsic::Subarray`, which only [`Self::system_view`] knows how to lower; here its
    /// sub-expressions are evaluated (JS order) and the view itself is dropped.
    pub(super) fn system_arg_effects(&mut self, a: &hir::Expr) {
        if let hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::Subarray),
            args,
        } = &a.kind
        {
            for sub in args {
                self.expr(sub);
            }
        } else {
            self.expr(a);
        }
    }

    /// Read the error the C function of `library` just left (`*__error()`, `*__errno_location()`,
    /// `*_errno()`; `GetLastError()` for a Windows library other than the C runtime) into the
    /// thread's slot, for a later `lastOsError()`, in this function or a caller. Does nothing on
    /// a target without one (WebAssembly).
    pub(super) fn capture_errno(&mut self, library: &str) {
        let Some(source) = c_abi::errno_source(&self.cx.target, library) else {
            return;
        };
        let v = match source {
            ErrnoSource::Accessor(symbol) => {
                let id = self.cx.extern_sym(symbol, vec![], Ty::Ptr, false);
                let ptr = self.temp(Ty::Ptr);
                self.call(
                    vir::Callee::Extern(id),
                    vec![],
                    Some(Place::local(ptr)),
                    false,
                );
                let at = Place {
                    local: ptr,
                    proj: vec![Proj::Deref(Ty::I32)],
                };
                let v = self.rvalue_temp(Ty::I32, Rvalue::Use(Operand::Copy(at)));
                self.rvalue_temp(Ty::U32, Rvalue::Cast(v, Ty::U32))
            }
            ErrnoSource::LastError => {
                let id = self.cx.extern_sym("GetLastError", vec![], Ty::U32, false);
                let out = self.temp(Ty::U32);
                self.call(
                    vir::Callee::Extern(id),
                    vec![],
                    Some(Place::local(out)),
                    false,
                );
                Operand::Copy(Place::local(out))
            }
        };
        // As lower/thread_local.rs names std's `OS_ERROR` (an `i32`: 32 bits, starting at 0).
        let id = crate::tls::stable_id(c_abi::ERRNO_SLOT);
        let set = TlsCall::Set {
            id,
            bits: 32,
            init: 0,
        };
        self.errno_tls_call(set, vec![v]);
    }

    /// The call of the thread-local extern `call`, inline in every backend.
    fn errno_tls_call(&mut self, call: TlsCall, args: Vec<Operand>) -> Operand {
        let (params, ret) = call.signature();
        let id = self.cx.extern_sym(&call.symbol(), params, ret, false);
        if ret == Ty::Unit {
            self.call(vir::Callee::Extern(id), args, None, false);
            return super::unit();
        }
        let out = self.temp(ret);
        self.call(
            vir::Callee::Extern(id),
            args,
            Some(Place::local(out)),
            false,
        );
        Operand::Copy(Place::local(out))
    }

    /// The data pointer and length (`u64`) of a buffer argument: an array's or a view's.
    pub(super) fn buffer(&mut self, arg: Option<Arg>) -> (Operand, Operand) {
        match arg {
            Some(Arg::Value(place, t)) => self.header_parts(&place, t),
            Some(Arg::View { place, t, outside }) => self.view_bytes(&place, t, &outside),
            Some(Arg::Sub {
                place,
                t,
                begin,
                end,
            }) => self.subarray_bytes(&place, t, begin, end),
            _ => ice("a buffer argument"),
        }
    }
}

impl FnLower<'_, '_> {
    /// A user call (the current `user_site`) of the standard-library function `def`, defined at
    /// `at`: remembered, so a C or host function the target lacks that it reaches is reported at
    /// this call (`unavailable_diagnostics`).
    pub(super) fn note_std_call(&mut self, def: DefId, at: velt_common::Span) {
        let site = self.cx.user_site;
        let Some(m) = self.cx.locs.as_ref() else {
            return;
        };
        if site != velt_common::Span::DUMMY && m.is_std(at) && !m.is_std(site) {
            self.cx.std_calls.entry(site).or_insert(def);
        }
    }
}

impl super::Cx<'_> {
    /// The error for each deferred type sema didn't reduce (`reduce_op`).
    pub(super) fn unreduced_diagnostics(&self) -> Vec<velt_common::Diagnostic> {
        if self.unreduced.is_empty() {
            return vec![];
        }
        // Where the types whose instances are one value are written.
        let span = (self.hir.arg_free_generic_fns.iter())
            .filter_map(|d| match self.hir.def(*d) {
                hir::Def::Interface(i) => Some(i.span),
                _ => None,
            })
            .min_by_key(|s| (s.file, s.lo))
            .unwrap_or(velt_common::Span::DUMMY);
        vec![velt_common::Diagnostic::error(
            "a function of this generic function type names a type-level form its call's type arguments don't evaluate: a value of the type at other type arguments is not supported yet (#943)",
            span,
        )]
    }

    /// The errors for calls of C and host functions the target lacks. One the standard library
    /// calls is reported at the user's call that reached it, as that call of std's function.
    pub(super) fn unavailable_diagnostics(&self) -> Vec<velt_common::Diagnostic> {
        let mut seen_decls = rustc_hash::FxHashSet::default();
        let mut seen_sites = rustc_hash::FxHashSet::default();
        let mut out = vec![];
        for &(d, caller, site) in &self.unavailable {
            if let hir::Def::Global(g) = self.hir.def(d) {
                let name = g.name.rsplit("::").next().unwrap_or(&g.name);
                let (what, note) = match &g.c_global {
                    Some(_) => (
                        "C variable",
                        "a C variable is in a C library, which this target doesn't link",
                    ),
                    None => (
                        "C constant",
                        "C constants come from C headers per target: this target has none",
                    ),
                };
                if seen_decls.insert(d) {
                    out.push(
                        velt_common::Diagnostic::error(
                            format!("{what} `{name}` is not available on {}", self.target),
                            g.span,
                        )
                        .with_note(note),
                    );
                }
                continue;
            }
            let hir::Def::ExternFn(x) = self.hir.def(d) else {
                continue;
            };
            let kind = match &x.host {
                Some(_) => "host function",
                None => "C function",
            };
            let in_std = self.locs.as_ref().is_some_and(|m| m.is_std(x.span));
            let entry = self.std_calls.get(&site).map(|&e| self.hir.def(e));
            let mut diag = match entry {
                Some(hir::Def::Fn(f)) if in_std => {
                    if !seen_sites.insert(site) {
                        continue;
                    }
                    velt_common::Diagnostic::error(
                        format!(
                            "{} is not available on {}",
                            self.std_name(&f.name),
                            self.target
                        ),
                        site,
                    )
                    .with_note(format!("it calls the {kind} `{}`", x.symbol))
                }
                _ => {
                    if !seen_decls.insert(d) {
                        continue;
                    }
                    let diag = velt_common::Diagnostic::error(
                        format!("{kind} `{}` is not available on {}", x.symbol, self.target),
                        x.span,
                    );
                    match caller.map(|c| self.hir.def(c)) {
                        Some(hir::Def::Fn(f)) => diag.with_note(format!("`{}` calls it", f.name)),
                        _ => diag,
                    }
                }
            };
            diag = diag.with_note(match (&x.host, &x.c) {
                (Some(_), _) => {
                    "only a JS host (wasm32-unknown-unknown) provides host functions".into()
                }
                (None, Some(c)) => self.c_note(&c.library),
                (None, None) => {
                    ice("an unavailable extern that is neither a host nor a C function")
                }
            });
            out.push(diag);
        }
        out.extend(self.c_clashes.iter().cloned());
        out
    }

    /// Why the C function of `library` can't be called here. On wasip1, std's calls of `c` can,
    /// so a call of `c` that lands here is a package's.
    fn c_note(&self, library: &str) -> String {
        if !super::c_call::wasi(&self.target) {
            return "C functions need a native target or wasm32-wasip1: the JS host has no C \
                    library, and std reaches the host through `declare host`"
                .into();
        }
        if library != "c" {
            return format!(
                "`{library}` has no library on {}: only the C library `c` (wasi-libc) is linked there",
                self.target
            );
        }
        format!(
            "C functions in packages need a native target on {} until their headers can be checked",
            self.target
        )
    }
}

impl super::Cx<'_> {
    /// A standard-library function as a program imports it: `std/fs/fd/open::openSync` is
    /// "`openSync` (velt/fs)", the module whose `index.velt` re-exports it.
    fn std_name(&self, qualified: &str) -> String {
        match qualified.rsplit_once("::") {
            Some((module, name)) => {
                let module = match self.locs.as_ref() {
                    Some(m) => m.owner_module(module),
                    None => std::borrow::Cow::Borrowed(module),
                };
                let module = module
                    .strip_prefix(velt_common::std_modules::CANONICAL_PREFIX)
                    .unwrap_or(&module);
                format!("`{name}` (velt/{module})")
            }
            None => format!("`{qualified}`"),
        }
    }
}

//! `@export("c_name")` (#2203 step 5, docs/internals/design/c-ffi.md "Exported functions"): a
//! top-level function C calls by that name, with the target's C calling convention. velt/ffi
//! declares the decorator (std/ffi/export.velt), and the compiler finds it through its hook role
//! (`decorator_export`), never by the name a use is written with.
//!
//! What may be exported is what a `declare c` function may take and return by value
//! (`collect::c_sig::value_ty`): integers, `f32`, `f64`, `bool`, `Ptr<T>` and `Ptr<T> | null`, and
//! `void` as the result. The function is not generic, async, a generator, a method or nested in
//! another, and throws nothing ([`throws`], after inference): C has no exceptions. Its name is a C
//! identifier outside what C and the runtime reserve, given to one function of the program, and
//! no C function the program declares has it. Each export is recorded in `Program::c_exports`,
//! which lowering builds a C function for (`velt_vir` `lower/c_export.rs`).

use velt_common::{std_hook, Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::{DefInfo, FnKind};
use crate::hir::{self, CTy, ConstTree, DecoratorTarget, DefId, TyKind};

/// The C library's and POSIX's functions and objects most likely to be chosen as a name: an
/// executable's definition of one would stand in for the C library's for every caller, the
/// runtime included (`free`, `write`), so they are refused with the names C and the runtime
/// reserve. A name the program's own `declare c` functions use is refused too.
const C_NAMES: &[&str] = &[
    "abort",
    "accept",
    "access",
    "alloca",
    "atexit",
    "atof",
    "atoi",
    "atol",
    "bind",
    "bsearch",
    "calloc",
    "chdir",
    "clock",
    "close",
    "connect",
    "dlclose",
    "dlerror",
    "dlopen",
    "dlsym",
    "dup",
    "dup2",
    "environ",
    "errno",
    "execve",
    "exit",
    "_exit",
    "fclose",
    "fcntl",
    "fflush",
    "fgets",
    "fopen",
    "fork",
    "fprintf",
    "fputs",
    "fread",
    "free",
    "fstat",
    "fwrite",
    "getenv",
    "getpid",
    "ioctl",
    "isatty",
    "kill",
    "listen",
    "longjmp",
    "lseek",
    "malloc",
    "memchr",
    "memcmp",
    "memcpy",
    "memmove",
    "memset",
    "mmap",
    "munmap",
    "open",
    "pipe",
    "poll",
    "printf",
    "pthread_create",
    "pthread_join",
    "pthread_self",
    "puts",
    "qsort",
    "raise",
    "rand",
    "read",
    "realloc",
    "recv",
    "rename",
    "select",
    "send",
    "setenv",
    "setjmp",
    "signal",
    "sigaction",
    "sleep",
    "snprintf",
    "socket",
    "sprintf",
    "stat",
    "stderr",
    "stdin",
    "stdout",
    "strcat",
    "strchr",
    "strcmp",
    "strcpy",
    "strdup",
    "strerror",
    "strlen",
    "strncmp",
    "strncpy",
    "strtod",
    "strtol",
    "system",
    "time",
    "unlink",
    "usleep",
    "vprintf",
    "wait",
    "waitpid",
    "write",
];

/// C's keywords (C23's included): a symbol may be spelled so, but no C code could declare it.
const C_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "auto",
    "bool",
    "break",
    "case",
    "char",
    "const",
    "constexpr",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "nullptr",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "struct",
    "switch",
    "thread_local",
    "true",
    "typedef",
    "typeof",
    "typeof_unqual",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
];

/// The `export` decorator's definition, when velt/ffi's module declaring it is loaded.
pub(super) fn role(cx: &Ctx) -> Option<DefId> {
    match cx.std_def(std_hook!(decorator_export)) {
        Some(Item::Def(d)) => Some(d),
        _ => None,
    }
}

/// Checks `@export` on function `d` (signature `sig`), if it has one, and records it in
/// `Ctx::c_exports`.
pub(super) fn check(cx: &mut Ctx, role: Option<DefId>, d: DefId, sig: &ast::FnSig) {
    let Some(role) = role else {
        return;
    };
    let Some(uses) = cx.decorator_uses.get(&DecoratorTarget::Def(d)) else {
        return;
    };
    let Some(u) = uses.iter().find(|u| u.def == role) else {
        return;
    };
    let (span, name) = match u.args.first() {
        Some(ConstTree::Str(s)) => (u.span, s.clone()),
        _ => return,
    };
    if !applies(cx, d, sig, span) {
        return;
    }
    let Some((params, ret)) = crossing(cx, d, sig) else {
        return;
    };
    if let Err(why) = c_name(&name) {
        cx.error(
            Diagnostic::error(
                format!("`{name}` can't be the C name of an `@export` function"),
                span,
            )
            .with_note(why),
        );
        return;
    }
    if let Some(first) = cx.c_exports.iter().find(|e| e.name == name) {
        let first = first.span;
        cx.error(
            Diagnostic::error(format!("two functions are exported to C as `{name}`"), span)
                .with_label(first, "first exported here"),
        );
        return;
    }
    if let Some(at) = declared_c(cx, &name) {
        cx.error(
            Diagnostic::error(
                format!("`{name}` is a C function the program declares, so it can't be the name of an `@export` function"),
                span,
            )
            .with_label(at, "declared here"),
        );
        return;
    }
    cx.c_exports.push(hir::CExport {
        def: d,
        name,
        params,
        ret,
        span,
    });
}

/// Is function `d` one C can call: top-level, with a body, neither generic, async nor a
/// generator? Reports why not at the decorator (`span`).
fn applies(cx: &mut Ctx, d: DefId, sig: &ast::FnSig, span: Span) -> bool {
    let f = cx.fn_info(d);
    let nested = cx.nested.iter().any(|n| n.item == Item::Def(d));
    let why = match f.kind {
        FnKind::Extern => Some("a function with a body, not to a `declare function`"),
        FnKind::Free if nested => Some("a top-level function, not to one declared in a body"),
        FnKind::Free => None,
        _ => Some("a top-level function"),
    };
    if let Some(why) = why {
        cx.err(format!("`@export` applies to {why}"), span);
        return false;
    }
    let why = if !f.generics.names.is_empty() {
        Some("a generic function: C calls one function, with one signature")
    } else if f.is_async || sig.is_async {
        Some("an async function: C can't await its promise")
    } else if f.is_generator || sig.is_generator {
        Some("a generator: C can't resume it")
    } else if sig.this_param.is_some() {
        Some("a function with a `this` parameter")
    } else {
        None
    };
    if let Some(why) = why {
        cx.error(
            Diagnostic::error(format!("`@export` doesn't apply to {why}"), span)
                .with_note("C calls an exported function directly, with the C calling convention"),
        );
        return false;
    }
    true
}

/// What crosses for each parameter of function `d` (signature `sig`) and for its result, as for
/// a `declare c` function; `None` when something can't, which is reported.
fn crossing(cx: &mut Ctx, d: DefId, sig: &ast::FnSig) -> Option<(Vec<CTy>, CTy)> {
    const ALLOWED: &str = "integers (`i8` … `u64`, `isize`, `usize`), `f32`, `f64`, `bool`, `Ptr<T>` and `Ptr<T> | null` (velt/ffi), as a `declare c` function's do, and `void` as the result";
    let tys: Vec<_> = cx.fn_info(d).params.iter().map(|p| p.ty).collect();
    let mut params = Vec::with_capacity(tys.len());
    let mut ok = true;
    for (p, ty) in sig.params.iter().zip(tys) {
        let what = if p.rest {
            Some("a rest parameter")
        } else if p.optional || p.default.is_some() {
            Some("an optional parameter")
        } else if p.pattern.is_some() {
            Some("a destructured parameter")
        } else {
            None
        };
        if let Some(what) = what {
            cx.err(
                format!("an `@export` function can't have {what}: C passes every argument"),
                p.span,
            );
            ok = false;
            continue;
        }
        match crate::collect::c_sig::value_ty(cx, ty) {
            Some(t) => params.push(t),
            None => {
                let shown = cx.display(ty);
                cx.error(
                    Diagnostic::error(
                        format!(
                            "parameter `{}` of an `@export` function has type `{shown}`, which can't cross from C",
                            p.name.name
                        ),
                        p.ty.span,
                    )
                    .with_note(format!("C passes {ALLOWED}")),
                );
                ok = false;
            }
        }
    }
    let ret_ty = cx.fn_info(d).ret;
    let ret = match cx.ty.kind(ret_ty) {
        TyKind::Unit => Some(CTy::Void),
        _ => crate::collect::c_sig::value_ty(cx, ret_ty),
    };
    let ret = match ret {
        Some(r) => r,
        None => {
            let shown = cx.display(ret_ty);
            let at = sig.ret.as_ref().map_or(sig.name.span, |t| t.span);
            cx.error(
                Diagnostic::error(
                    format!("an `@export` function returns `{shown}`, which can't cross to C"),
                    at,
                )
                .with_note(format!("it returns {ALLOWED}")),
            );
            return None;
        }
    };
    ok.then_some((params, ret))
}

/// Why `name` can't name a C symbol, if it can't.
fn c_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let ident = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ident {
        return Err(
            "a C name is a C identifier: ASCII letters, digits and `_`, not starting with a digit"
                .into(),
        );
    }
    if name.len() > 255 {
        return Err("a C name has at most 255 characters".into());
    }
    if name.starts_with("velt_") || name.starts_with("VELT_") {
        return Err("names starting with `velt_` are the Velt runtime's".into());
    }
    let b = name.as_bytes();
    if b[0] == b'_'
        && b.get(1)
            .is_some_and(|c| *c == b'_' || c.is_ascii_uppercase())
    {
        return Err("C reserves names starting with `__` or `_` and a capital letter".into());
    }
    if name == "main" {
        return Err("`main` is the program's entry, which the runtime defines".into());
    }
    if C_KEYWORDS.contains(&name) {
        return Err("it is a keyword of C, so no C code could declare it".into());
    }
    if C_NAMES.contains(&name) {
        return Err("it is a function or object of the C library, which the program's definition would replace for every caller, the runtime's included".into());
    }
    Ok(())
}

/// Where the program declares C function `name` (`declare c`, any `$suffix`), if it does.
fn declared_c(cx: &Ctx, name: &str) -> Option<Span> {
    cx.info.iter().find_map(|i| match i {
        DefInfo::Fn(f) if f.c.is_some() => {
            let declared = hir::declared_fn_name(&f.name);
            let base = declared.split('$').next().unwrap_or(declared);
            (base == name).then_some(f.name_span)
        }
        _ => None,
    })
}

/// After throws inference: an exported function that may throw is an error, since C has no
/// exceptions to receive one.
pub(crate) fn throws(cx: &mut Ctx) {
    let found: Vec<(Span, String, hir::TyId)> = (cx.c_exports.iter())
        .filter_map(|e| {
            let t = cx.fn_info(e.def).throws?;
            (t != cx.ty.never).then(|| (e.span, e.name.clone(), t))
        })
        .collect();
    for (span, name, t) in found {
        let shown = cx.display(t);
        cx.error(
            Diagnostic::error(
                format!("the `@export` function `{name}` may throw `{shown}`, which can't reach C"),
                span,
            )
            .with_note("C has no exceptions: catch the error in the function and return a result C can read"),
        );
    }
}

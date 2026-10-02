//! The compiler's side of native packages (docs/internals/contracts/native_abi.md):
//!
//! - [`check_declares`]: every `declare function` of a package with native code must name one of
//!   **its own** library's exports, and its lowered signature must equal the signature the SDK
//!   recorded for that export exactly. A mismatch would be memory corruption at run time, so it
//!   is a compile error. No package may declare another package's export, and a package with
//!   native code may not declare `velt_rt_*` runtime functions (only std does). `IoResult` and
//!   `IoStatus` are std's (`velt:io`) types, identified by definition, not by name.
//! - [`inits`]: the libraries `velt_main` initializes before `main`.
//! - [`links`]: what the linker adds per library.

use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Diagnostics, SourceMap};
use velt_sema::hir::{self, Def, DefId, FloatTy, IntTy, TyId, TyKind};
use vpm::PackageGraph;

/// The definitions of std's `IoResult` and `IoStatus` (`velt:io`), if the program has them.
#[derive(Clone, Copy, Debug, Default)]
pub struct IoTypes {
    io_result: Option<DefId>,
    io_status: Option<DefId>,
}

impl IoTypes {
    /// Find them: the ADTs named `IoResult`/`IoStatus` defined in `<std_root>/io.vlt`.
    pub fn find(p: &hir::Program, sm: &SourceMap, std_root: Option<&Path>) -> IoTypes {
        let mut io = IoTypes::default();
        let Some(file) = std_root.map(|r| canonical(&r.join("io.vlt"))) else {
            return io;
        };
        for (i, def) in p.defs.iter().enumerate() {
            let Def::Adt(a) = def else { continue };
            if canonical(&sm.get(a.span.file).path) != file {
                continue;
            }
            let id = Some(DefId(i as u32));
            match a.name.rsplit("::").next() {
                Some("IoResult") => io.io_result = id,
                Some("IoStatus") => io.io_status = id,
                _ => {}
            }
        }
        io
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The signature of `f` in the notation of native_abi.md (`(string,u32)->IoResult<u64>`, with
/// `async ` in front for `declare async function`), or why one of its types cannot cross the
/// native boundary.
pub fn signature(p: &hir::Program, io: IoTypes, f: &hir::ExternFnDef) -> Result<String, String> {
    let params = f
        .params
        .iter()
        .map(|&t| param(p, t))
        .collect::<Result<Vec<_>, _>>()?;
    let ret = if f.is_async {
        match p.types.kind(f.ret) {
            TyKind::Promise(t, _) => result(p, io, *t)?,
            _ => return Err("an async declaration must return a `Promise<T>`".into()),
        }
    } else {
        result(p, io, f.ret)?
    };
    let prefix = if f.is_async { "async " } else { "" };
    Ok(format!("{prefix}({})->{ret}", params.join(",")))
}

fn scalar(p: &hir::Program, t: TyId) -> Option<&'static str> {
    Some(match p.types.kind(t) {
        TyKind::Bool => "bool",
        TyKind::Int(i) => match i {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::ISize | IntTy::USize => return None,
        },
        TyKind::Float(FloatTy::F32) => "f32",
        TyKind::Float(FloatTy::F64) => "f64",
        _ => return None,
    })
}

/// `string`, `u8[]` or a scalar.
fn value(p: &hir::Program, t: TyId) -> Option<&'static str> {
    match p.types.kind(t) {
        TyKind::Str => Some("string"),
        TyKind::Array(e) if matches!(p.types.kind(*e), TyKind::Int(IntTy::U8)) => Some("u8[]"),
        _ => scalar(p, t),
    }
}

fn param(p: &hir::Program, t: TyId) -> Result<String, String> {
    value(p, t).map(str::to_string).ok_or_else(|| {
        format!(
            "parameters of a native function must be scalars, `string` or `u8[]`, not `{}`",
            ty_name(p, t)
        )
    })
}

fn result(p: &hir::Program, io: IoTypes, t: TyId) -> Result<String, String> {
    if matches!(p.types.kind(t), TyKind::Unit) {
        return Ok("void".into());
    }
    if let Some(v) = value(p, t) {
        return Ok(v.into());
    }
    if let TyKind::Adt(d, args) = p.types.kind(t) {
        let d = Some(*d);
        if d == io.io_status && args.is_empty() {
            return Ok("IoStatus".into());
        }
        if let ([v], true) = (args.as_slice(), d == io.io_result) {
            if let Some(v) = value(p, *v) {
                return Ok(format!("IoResult<{v}>"));
            }
        }
    }
    Err(format!(
        "a native function returns `void`, a scalar, `string`, `u8[]`, `IoStatus` or \
         `IoResult<T>` of one of those, not `{}`",
        ty_name(p, t)
    ))
}

fn ty_name(p: &hir::Program, t: TyId) -> String {
    match p.types.kind(t) {
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => a.name.clone(),
            _ => "type".into(),
        },
        TyKind::FnPtr { .. } | TyKind::Closure(_) => "function".into(),
        TyKind::Array(_) => "array".into(),
        TyKind::Int(IntTy::ISize) => "isize".into(),
        TyKind::Int(IntTy::USize) => "usize".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// Check every `declare function` of the program's packages against their native libraries
/// (see the module docs); reports errors into `diags`.
pub fn check_declares(
    p: &hir::Program,
    sm: &SourceMap,
    std_root: Option<&Path>,
    graph: &PackageGraph,
    diags: &mut Diagnostics,
) {
    if graph.natives().next().is_none() {
        return;
    }
    let io = IoTypes::find(p, sm, std_root);
    for def in &p.defs {
        let Def::ExternFn(f) = def else { continue };
        let file = &sm.get(f.span.file).path;
        let Some(pkg) = graph.package_of(file) else {
            continue;
        };
        let owner = graph
            .natives()
            .find(|(_, lib)| lib.meta.exports.contains_key(&f.symbol));
        let Some(lib) = &pkg.native else {
            if let Some((other, _)) = owner {
                diags.push(Diagnostic::error(
                    format!(
                        "`{}` is exported by the native library of package `{}`; only that package may declare it",
                        f.symbol, other.name
                    ),
                    f.span,
                ));
            }
            continue;
        };
        let what = format!("`{} {}`", lib.meta.package, lib.meta.version);
        if f.symbol.starts_with("velt_rt_") {
            diags.push(Diagnostic::error(
                format!(
                    "`{}` is a runtime function: a package with native code may only declare its own library's exports",
                    f.symbol
                ),
                f.span,
            ));
            continue;
        }
        let Some(expected) = lib.meta.exports.get(&f.symbol) else {
            let mut d = Diagnostic::error(
                format!(
                    "`{}` is not exported by the native library of {what}",
                    f.symbol
                ),
                f.span,
            );
            if let Some(close) = closest(&f.symbol, lib.meta.exports.keys()) {
                d = d.with_note(format!("did you mean `{close}`?"));
            }
            diags.push(d);
            continue;
        };
        match signature(p, io, f) {
            Ok(actual) if actual == *expected => {}
            Ok(actual) => diags.push(
                Diagnostic::error(
                    format!(
                        "`declare` of `{}` does not match the native library of {what}",
                        f.symbol
                    ),
                    f.span,
                )
                .with_note(format!("the library exports `{expected}`"))
                .with_note(format!("this declares     `{actual}`")),
            ),
            Err(why) => diags.push(Diagnostic::error(why, f.span)),
        }
    }
}

fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a String>) -> Option<&'a String> {
    let names: Vec<&String> = candidates.collect();
    let strs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let found = crate::cli::suggest::closest(name, &strs)?;
    names.into_iter().find(|n| n.as_str() == found)
}

/// The native libraries `velt_main` initializes, in graph order.
pub fn inits(graph: Option<&PackageGraph>) -> Vec<velt_vir::NativeInit> {
    graph
        .into_iter()
        .flat_map(|g| g.natives())
        .map(|(_, lib)| velt_vir::NativeInit {
            package: lib.meta.package.clone(),
            symbol: lib.init_symbol(),
        })
        .collect()
}

/// What the linker adds per native library: the shared library in debug builds (a fast link,
/// like the shared runtime), the prelinked object in release builds so the executable stays
/// self-contained (where there is one: not on Windows).
pub fn links(graph: Option<&PackageGraph>, release: bool) -> Vec<velt_link::NativeLink> {
    graph
        .into_iter()
        .flat_map(|g| g.natives())
        .map(|(_, lib)| velt_link::NativeLink {
            shared: lib.shared_lib(),
            import_lib: lib.import_lib(),
            static_obj: lib.static_obj().filter(|_| release),
        })
        .collect()
}

#[cfg(test)]
mod tests;

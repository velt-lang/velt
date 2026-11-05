//! The compiler's side of native packages (docs/internals/contracts/native_abi.md):
//!
//! - [`check_declares`]: outside the standard library, a `declare function` must name one of the
//!   exports of **its own** package's native library, and its lowered signature must equal the
//!   signature the SDK recorded for that export exactly. Anything else (a root program or a
//!   package without native code declaring `free`, another package's export, a mismatch) would
//!   let ordinary code call arbitrary C symbols or corrupt memory, so it is a compile error.
//!   `IoResult` and `IoStatus` are std's (`velt/io`) types, identified by definition, not by name.
//! - [`inits`]: the libraries `velt_main` initializes before `main`.
//! - [`links`]: what the linker adds per library.
//! - [`check_trust`]: `unsafe` code and the raw FFI API only in std and allowed native packages,
//!   each piece with a `// SAFETY:` comment (docs/internals/design/c-ffi.md); and what `velt
//!   check` lists about them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap};
use velt_sema::hir::{self, Def, DefId, FloatTy, IntTy, TyId, TyKind};
use vpm::PackageGraph;

/// The definitions of std's `IoResult` and `IoStatus` (`velt/io`), if the program has them.
#[derive(Clone, Copy, Debug, Default)]
pub struct IoTypes {
    io_result: Option<DefId>,
    io_status: Option<DefId>,
}

impl IoTypes {
    /// Find them: the ADTs `IoResult` in `<std_root>/native/result.vlt` and `IoStatus` in
    /// `<std_root>/native/status.vlt`.
    pub fn find(p: &hir::Program, sm: &SourceMap, std_root: Option<&Path>) -> IoTypes {
        let mut io = IoTypes::default();
        let Some(native) = std_root.map(|r| r.join("native")) else {
            return io;
        };
        let result = canonical(&native.join("result.vlt"));
        let status = canonical(&native.join("status.vlt"));
        for (i, def) in p.defs.iter().enumerate() {
            let Def::Adt(a) = def else { continue };
            let file = canonical(&sm.get(a.span.file).path);
            let id = Some(DefId(i as u32));
            match a.name.rsplit("::").next() {
                Some("IoResult") if file == result => io.io_result = id,
                Some("IoStatus") if file == status => io.io_status = id,
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

/// A value's name in a signature (the SDK's `Value::SIG`): a scalar, `string`, `T[]` of a value,
/// or a `struct` of values written `{name:type,...}` in field order (Velt lays a struct out in
/// that order, as the SDK's `#[repr(C)]` derive does). `None` when it cannot cross: classes,
/// interfaces, unions, optional or private fields, generic or recursive structs.
fn value(p: &hir::Program, t: TyId) -> Option<String> {
    value_in(p, t, &mut Vec::new())
}

fn value_in(p: &hir::Program, t: TyId, open: &mut Vec<DefId>) -> Option<String> {
    match p.types.kind(t) {
        TyKind::Str => Some("string".into()),
        TyKind::Array(e) => Some(format!("{}[]", value_in(p, *e, open)?)),
        TyKind::Adt(d, args) if args.is_empty() => {
            let Def::Adt(a) = p.def(*d) else { return None };
            if !a.declared_struct || a.fields.is_empty() || open.contains(d) {
                return None;
            }
            open.push(*d);
            let fields: Option<Vec<String>> = a
                .fields
                .iter()
                .map(|f| {
                    if f.optional || f.private {
                        return None;
                    }
                    Some(format!("{}:{}", f.name, value_in(p, f.ty, open)?))
                })
                .collect();
            open.pop();
            Some(format!("{{{}}}", fields?.join(",")))
        }
        _ => scalar(p, t).map(str::to_string),
    }
}

/// What can cross, for messages.
const VALUES: &str = "scalars, `string`, arrays of values and `struct`s whose fields are values";

fn param(p: &hir::Program, t: TyId) -> Result<String, String> {
    value(p, t).ok_or_else(|| {
        format!(
            "parameters of a native function must be {VALUES}, not `{}`",
            ty_name(p, t)
        )
    })
}

fn result(p: &hir::Program, io: IoTypes, t: TyId) -> Result<String, String> {
    if matches!(p.types.kind(t), TyKind::Unit) {
        return Ok("void".into());
    }
    // std's `IoStatus` and `IoResult<T>` are structs too: named as themselves, first.
    if let TyKind::Adt(d, args) = p.types.kind(t) {
        let d = Some(*d);
        if d == io.io_status && args.is_empty() {
            return Ok("IoStatus".into());
        }
        if d == io.io_result {
            if let [v] = args.as_slice() {
                if let Some(v) = value(p, *v) {
                    return Ok(format!("IoResult<{v}>"));
                }
            }
        }
    }
    if let Some(v) = value(p, t) {
        return Ok(v);
    }
    Err(format!(
        "a native function returns `void`, `IoStatus`, a value or `IoResult<T>` of a value \
         (values: {VALUES}), not `{}`",
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

/// Check every `declare function` outside the standard library (`std_files`: the files of std
/// modules) against the native library of its own package (`graph`, `None` for a program without
/// packages), and every `declare system function` against the table of system functions of
/// `target` (system.rs); see the module docs. Reports errors into `diags`, and returns the C
/// libraries that passed the header check with their records, for [`note_checked`].
pub fn check_declares(
    p: &hir::Program,
    sm: &SourceMap,
    std_root: Option<&Path>,
    graph: Option<&PackageGraph>,
    std_files: &HashSet<FileId>,
    target: &str,
    diags: &mut Diagnostics,
) -> CChecked {
    let io = IoTypes::find(p, sm, std_root);
    let mut checked = c_decls::Checked::default();
    for def in &p.defs {
        let Def::ExternFn(f) = def else { continue };
        if f.system.is_some() {
            system::check(p, f, target, diags);
            continue;
        }
        // The JS host provides it (sema checked what crosses; lowering, the target).
        if f.host.is_some() {
            continue;
        }
        if let Some(c) = &f.c {
            c_decls::check(f, c, sm, graph, std_files, target, &mut checked, diags);
            continue;
        }
        if std_files.contains(&f.span.file) {
            continue;
        }
        let file = &sm.get(f.span.file).path;
        let pkg = graph.and_then(|g| g.package_of(file));
        let Some(lib) = pkg.and_then(|p| p.native.as_ref()) else {
            let mut d = not_native(f, pkg.map(|p| p.name.as_str()), graph);
            if let Some(h) = system::hint(f) {
                d = d.with_note(h);
            }
            diags.push(d);
            continue;
        };
        let what = format!("`{} {}`", lib.meta.package, lib.meta.version);
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
    headers::check(p, sm, graph, std_files, target, diags)
}

/// The error for a `declare function` in a module whose package (`pkg`; `None`: a program
/// without a package) has no native library.
fn not_native(f: &hir::ExternFnDef, pkg: Option<&str>, graph: Option<&PackageGraph>) -> Diagnostic {
    let whose = match pkg {
        Some(name) => format!("package `{name}` has no native library"),
        None => "this program is not in a package with a native library".into(),
    };
    let kind = if f.is_async {
        "declare async function"
    } else {
        "declare function"
    };
    let d = Diagnostic::error(
        format!("`{kind} {}` is not allowed: {whose}", f.symbol),
        f.span,
    )
    .with_note(
        "outside the standard library, `declare function` may only name an export of the \
         package's own native library",
    );
    let owner = graph
        .into_iter()
        .flat_map(|g| g.natives())
        .find(|(_, lib)| lib.meta.exports.contains_key(&f.symbol));
    match owner {
        Some((other, _)) => d.with_note(format!(
            "`{}` is exported by the native library of package `{}`: import that package's API instead",
            f.symbol, other.name
        )),
        None => d.with_note(
            "to call C code, give the package a native library (`native` in package.vlt; see \
             Packages with native code), or use the standard library's API",
        ),
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

/// What the linker adds for a program: its packages' native libraries (with `crates`; a source
/// generator links none), then the C libraries of `c_libs` built from `sources`.
pub fn program_links(
    graph: Option<&PackageGraph>,
    c_libs: &[String],
    crates: bool,
    release: bool,
    target: &str,
) -> Result<Vec<velt_link::NativeLink>, String> {
    let mut out = match crates {
        true => links(graph, release),
        false => vec![],
    };
    out.extend(c_sources::links(graph, c_libs, release, target)?);
    Ok(out)
}

/// What the linker adds per native library: the shared library in debug builds (a fast link,
/// like the shared runtime), the prelinked object in release builds so the executable stays
/// self-contained (where there is one: not on Windows).
pub fn links(graph: Option<&PackageGraph>, release: bool) -> Vec<velt_link::NativeLink> {
    graph
        .into_iter()
        .flat_map(|g| g.natives())
        .map(|(_, lib)| {
            let static_obj = lib.static_obj().filter(|_| release);
            velt_link::NativeLink {
                shared: lib.shared_lib(),
                import_lib: lib.import_lib(),
                system_libs: match static_obj {
                    Some(_) => lib.meta.system_libs.clone().unwrap_or_default(),
                    None => Vec::new(),
                },
                static_obj,
            }
        })
        .collect()
}

mod c_decls;
mod c_sources;
mod headers;
pub use c_decls::{link_args as c_link_args, links as c_links};
pub use c_sources::{dev_libraries as c_dev_libraries, system_libraries as c_system_libraries};
pub use headers::{
    read_constants as read_c_constants, stamp_inputs as c_stamp_inputs, zig_target,
    Checked as CChecked, Constants as CConstants,
};
pub use report::note_checked;
mod report;
mod system;
pub use report::{json as native_json, render as render_native, NativePackage};
mod trust;
mod unsafe_sites;

/// Where `module` first has unsafe code, if it has any: what a generator's output can't have, as
/// the trust check reads only the loaded modules.
pub(crate) fn first_unsafe_code(module: &velt_syntax::ast::Module) -> Option<velt_common::Span> {
    unsafe_sites::find(module).sites.first().map(|s| s.span)
}

/// [`trust`]'s checks of `modules` (errors into `diags`), and the packages with native code the
/// program allows, with their unsafe code, for `velt check` to list.
pub fn check_trust(
    modules: &[velt_sema::SourceModule],
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    target: &str,
    diags: &mut Diagnostics,
) -> Vec<NativePackage> {
    let sites = trust::check(modules, sm, graph, diags);
    report::native_packages(graph, &sites, sm, target)
}

#[cfg(test)]
mod tests;

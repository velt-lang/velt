//! The compiler's intrinsics as std declares them (docs/internals/design/std-layout.md §10):
//! `std/lang/intrinsics/*.vlt` writes each one as `export declare intrinsic function
//! arraySwap<T>(xs: Mut<T[]>, i: usize, j: usize): void;`, and [`TABLE`] maps the declared name
//! to what the compiler does for a call. The declaration is the signature calls are checked
//! against (`body::expr::intrinsic_call`); this table holds no types. std imports the functions
//! like any other, and since `velt/lang` is internal, no program or package can.
//!
//! A declaration whose name the table lacks is an ICE-level error where it is declared; a table
//! entry without a declaration is caught by the test below, which reads std's declarations.

use crate::hir::Intrinsic;

/// What a call of a declared intrinsic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeclaredIntrinsic {
    /// An [`Intrinsic`] call, its arguments checked against the declaration.
    Op(Intrinsic),
    /// `targetFeature("sha-arm")`: the statement that gates a std function on a CPU extension
    /// (`body::gated`).
    TargetFeature,
    /// `anyToPrimitive(value, hint, at)` in the prelude's `__anyToPrimitive`: the per-class
    /// dispatch sema writes out (`body::expr::any_prim`).
    AnyToPrimitive,
    /// A `declare intrinsic function` that names none, or one outside std/lang/intrinsics: the
    /// declaration was reported, and calls of it check their arguments and are errors.
    Invalid,
}

use DeclaredIntrinsic::{AnyToPrimitive, Op, TargetFeature};
use Intrinsic as I;

/// Every declared intrinsic: its name in `std/lang/intrinsics` and what a call of it is.
pub(crate) const TABLE: &[(&str, DeclaredIntrinsic)] = &[
    // any.vlt
    ("anyDelete", Op(I::AnyDelete)),
    ("anyToPrimitive", AnyToPrimitive),
    ("proxyArrayCall", Op(I::ProxyArrayCall)),
    ("proxyArrayHole", Op(I::ProxyArrayHole)),
    // array.vlt
    ("arrayMove", Op(I::ArrayMove)),
    ("arrayPrefetch", Op(I::ArrayPrefetch)),
    ("arrayRemove", Op(I::ArrayRemove)),
    ("arrayReserve", Op(I::ArrayReserve)),
    ("arrayRotateLeft", Op(I::ArrayRotateLeft)),
    ("arrayRotateRight", Op(I::ArrayRotateRight)),
    ("arraySetLen", Op(I::ArraySetLen)),
    ("arraySwap", Op(I::ArraySwap)),
    ("arraySwapNested", Op(I::ArraySwapNested)),
    ("arrayTruncate", Op(I::ArrayTruncate)),
    ("arrayWithCapacity", Op(I::ArrayWithCapacity)),
    // atomic.vlt
    ("atomicCas", Op(I::AtomicCas)),
    ("atomicCasWeak", Op(I::AtomicCasWeak)),
    ("atomicFence", Op(I::AtomicFence)),
    ("atomicLoad", Op(I::AtomicLoad)),
    ("atomicRmw", Op(I::AtomicRmw)),
    ("atomicSpinHint", Op(I::AtomicSpinHint)),
    ("atomicStore", Op(I::AtomicStore)),
    // channel.vlt
    ("chanReceive", Op(I::ChanReceive)),
    ("chanSend", Op(I::ChanSend)),
    ("chanTryReceive", Op(I::ChanTryReceive)),
    ("chanTrySend", Op(I::ChanTrySend)),
    // compare.vlt
    ("deepEq", Op(I::DeepEq)),
    ("eq", Op(I::Eq)),
    ("hash", Op(I::Hash)),
    ("includesEq", Op(I::IncludesEq)),
    ("keyEq", Op(I::KeyEq)),
    // db.vlt
    ("dbBind", Op(I::DbBind)),
    ("readRowByColumn", Op(I::ReadRow)),
    ("readRowsByColumn", Op(I::ReadRows)),
    // generator.vlt
    ("asyncGeneratorDispose", Op(I::AsyncGeneratorDispose)),
    ("asyncGeneratorResume", Op(I::AsyncGeneratorResume)),
    ("asyncGeneratorReturn", Op(I::AsyncGeneratorReturn)),
    ("asyncGeneratorValue", Op(I::AsyncGeneratorValue)),
    ("generatorResume", Op(I::GeneratorResume)),
    ("generatorResumeWith", Op(I::GeneratorResumeWith)),
    ("generatorReturn", Op(I::GeneratorReturn)),
    ("generatorThrow", Op(I::GeneratorThrow)),
    ("generatorValue", Op(I::GeneratorValue)),
    // json.vlt
    ("jsonParse", Op(I::JsonParse)),
    ("jsonParseCursor", Op(I::JsonParseCursor)),
    ("jsonStringify", Op(I::JsonStringify)),
    // math.vlt
    ("abs", Op(I::FAbs)),
    ("ceil", Op(I::Ceil)),
    ("floor", Op(I::Floor)),
    ("round", Op(I::Round)),
    ("sqrt", Op(I::Sqrt)),
    ("trunc", Op(I::Trunc)),
    // ownership.vlt
    ("share", Op(I::Share)),
    ("transfer", Op(I::Transfer)),
    // report.vlt
    ("errorText", Op(I::ErrorText)),
    ("inspect", Op(I::Inspect)),
    ("throwLoc", Op(I::ThrowLoc)),
    // simd.vlt
    ("strLoadPartialU8x16", Op(I::StrLoadPartialU8x16)),
    ("strLoadU8x16", Op(I::StrLoadU8x16)),
    ("targetFeature", TargetFeature),
    // string.vlt
    ("strByteAt", Op(I::StrByteAt)),
    ("strByteLen", Op(I::StrByteLen)),
    ("strCharCodeAt", Op(I::StrCharCodeAt)),
    ("strLen", Op(I::StrLen)),
    // symbol.vlt
    ("symbolRegistry", Op(I::SymbolRegistry)),
    ("symbolStatic", Op(I::SymbolStatic)),
    // task.vlt
    ("firstFulfilled", Op(I::PromiseAny)),
    ("spawnHandled", Op(I::SpawnHandled)),
    // thread_local.vlt
    ("threadLocalGet", Op(I::ThreadLocalGet)),
    ("threadLocalSet", Op(I::ThreadLocalSet)),
    // types.vlt
    ("fnCapturesNothing", Op(I::FnCapturesNothing)),
    ("holdsNull", Op(I::HoldsNull)),
    ("mayAlias", Op(I::MayAlias)),
    ("needsDrop", Op(I::NeedsDrop)),
    ("needsTransfer", Op(I::NeedsTransfer)),
    ("nullAs", Op(I::NullAs)),
    // weak.vlt
    ("weakMapDelete", Op(I::WeakMapDelete)),
    ("weakMapGet", Op(I::WeakMapGet)),
    ("weakMapHas", Op(I::WeakMapHas)),
    ("weakMapSet", Op(I::WeakMapSet)),
    ("weakMapValue", Op(I::WeakMapValue)),
    ("weakRefDeref", Op(I::WeakRefDeref)),
    ("weakRefNew", Op(I::WeakRefNew)),
];

/// The intrinsic std declares as `name`.
pub(crate) fn declared(name: &str) -> Option<DeclaredIntrinsic> {
    TABLE.iter().find(|(n, _)| *n == name).map(|&(_, d)| d)
}

/// What the `declare intrinsic function` `sig` in module `m` declares, after reporting a
/// declaration that is not allowed there or names no intrinsic.
pub(crate) fn declare(
    cx: &mut crate::ctx::Ctx,
    m: usize,
    sig: &velt_syntax::ast::FnSig,
) -> DeclaredIntrinsic {
    let name = &sig.name.name;
    if !cx.scopes[m].is_std {
        cx.error(
            velt_common::Diagnostic::error(
                format!("`declare intrinsic function {name}` is not allowed: only the standard library declares intrinsics"),
                sig.name.span,
            )
            .with_note("an intrinsic is an operation of the compiler itself; std implements its public API with them, which is what programs and packages use"),
        );
        return DeclaredIntrinsic::Invalid;
    }
    let path = cx.modules[m].path.as_str();
    let dir = velt_common::std_module!(lang_intrinsics);
    if path != dir && !path.starts_with(velt_common::std_dir!(lang_intrinsics)) {
        cx.err(
            format!("ICE: the intrinsic `{name}` is declared outside std/lang/intrinsics"),
            sig.name.span,
        );
        return DeclaredIntrinsic::Invalid;
    }
    match declared(name) {
        Some(d) => d,
        None => {
            cx.err(
                format!("ICE: no compiler intrinsic is named `{name}`: velt_sema's intrinsic table (intrinsic_decls.rs) and std/lang/intrinsics must declare the same ones"),
                sig.name.span,
            );
            DeclaredIntrinsic::Invalid
        }
    }
}

/// The name std declares intrinsic `i` with, for messages about a call the compiler makes.
pub(crate) fn name_of(i: Intrinsic) -> Option<&'static str> {
    TABLE.iter().find(|(_, d)| *d == Op(i)).map(|&(n, _)| n)
}

/// The modes of a declared intrinsic's parameters `ps`: a parameter whose written type is
/// `Mut<T>`, `Owned<T>` or `Copied<T>` (std/lang/intrinsics/modes.vlt, aliases of `T` that
/// module `m` imports from there) is a mutable borrow, taken over or copied; the others keep the
/// mode of their type (copied for numbers, `bool` and `symbol`, else borrowed).
pub(crate) fn param_modes(
    cx: &mut crate::ctx::Ctx,
    m: usize,
    sig: &velt_syntax::ast::FnSig,
    ps: &mut [crate::defs::ParamSig],
) {
    use crate::hir::PassMode;
    use velt_syntax::ast::TypeExprKind;
    for (p, written) in ps.iter_mut().zip(&sig.params) {
        let TypeExprKind::Named { path, args } = &written.ty.kind else {
            continue;
        };
        let ([name], [_]) = (path.as_slice(), args.as_slice()) else {
            continue;
        };
        let mode = match name.name.as_str() {
            "Mut" => PassMode::BorrowMut,
            "Owned" => PassMode::Owned,
            "Copied" => PassMode::Copy,
            _ => continue,
        };
        let from_modes = match cx.scopes[m].items.get(&name.name) {
            Some(&crate::ctx::Item::Alias(a)) => {
                let path = &cx.modules[cx.aliases[a as usize].module].path;
                path.starts_with(velt_common::std_dir!(lang_intrinsics))
            }
            _ => false,
        };
        if from_modes {
            p.mode = mode;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use velt_syntax::ast::ItemKind;

    use super::TABLE;

    fn std_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std")
    }

    fn vlt_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("std is readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                vlt_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "vlt") {
                out.push(path);
            }
        }
    }

    /// The `declare intrinsic function` names of every std file, by file below `std/`.
    fn declarations() -> Vec<(String, String, bool)> {
        let root = std_root();
        let mut files = vec![];
        vlt_files(&root, &mut files);
        let mut out = vec![];
        for path in files {
            let src = std::fs::read_to_string(&path).expect("std file is readable");
            let rel = path.strip_prefix(&root).unwrap_or(&path);
            let rel = rel.to_string_lossy().replace('\\', "/");
            let (module, diags) = velt_syntax::parse_file(velt_common::FileId(0), &src);
            let errors: Vec<_> = diags.iter().filter(|d| d.is_error()).collect();
            assert!(errors.is_empty(), "std/{rel} does not parse: {errors:?}");
            for item in &module.items {
                if let ItemKind::ExternFn(sig) = &item.kind {
                    if sig.is_intrinsic {
                        out.push((rel.clone(), sig.name.name.clone(), item.exported));
                    }
                }
            }
        }
        out
    }

    /// The table and std's declarations name the same intrinsics, each once, all declared in
    /// std/lang/intrinsics and exported: a missing or an extra one fails here.
    #[test]
    fn table_matches_the_declarations() {
        let mut table = BTreeSet::new();
        for (name, _) in TABLE {
            assert!(table.insert(*name), "`{name}` is in the table twice");
        }
        let mut declared = BTreeSet::new();
        for (file, name, exported) in declarations() {
            assert!(
                file.starts_with("lang/intrinsics/"),
                "std/{file} declares the intrinsic `{name}`: intrinsics are declared in std/lang/intrinsics"
            );
            assert!(
                exported,
                "std/{file}: the intrinsic `{name}` is not exported"
            );
            assert!(
                declared.insert(name.clone()),
                "the intrinsic `{name}` is declared twice"
            );
        }
        let missing: Vec<_> = table.iter().filter(|n| !declared.contains(**n)).collect();
        let extra: Vec<_> = declared
            .iter()
            .filter(|n| !table.contains(n.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "in velt_sema's intrinsic table but not declared in std/lang/intrinsics: {missing:?}"
        );
        assert!(
            extra.is_empty(),
            "declared in std/lang/intrinsics but not in velt_sema's intrinsic table: {extra:?}"
        );
    }

    /// Two names never mean the same operation (`name_of` gives one).
    #[test]
    fn each_operation_has_one_name() {
        for (i, (a, x)) in TABLE.iter().enumerate() {
            for (b, y) in &TABLE[i + 1..] {
                assert!(x != y, "`{a}` and `{b}` are the same intrinsic");
            }
        }
    }
}

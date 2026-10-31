//! The compiler's hooks (velt_common `std_hooks.rs`, docs/internals/design/std-layout.md §10b):
//! every entry of the table names a definition of std with the signature the table gives, and
//! no program can name one.

mod common;

use common::programs::{err_src, ok_src};
use velt_common::std_hooks::HOOKS;
use velt_sema::hir::{Def, FloatTy, IntTy, Nothing, Program, TyId, TyKind};

/// A program that loads every module defining a hook: the prelude loads `velt/lang/hooks`, and
/// the others come with what they implement (`Proxy`, `Set`, `Uint8Array`, `RegExp`; velt/lang/this_fn,
/// which the driver's loader adds for a function with `this`, and this test's loader does not).
const LOADS_ALL: &str =
    "import \"velt/lang/this_fn\";\nimport \"velt/test/registry\";\nimport \"velt/test/run\";\n\
    function main() {\n  const p = new Proxy({}, {});\n  const s = new Set<number>();\n  \
    const b = new Uint8Array(1);\n  const r = new RegExp(\"a\");\n}\n";

/// Type parameter `i` as the table writes it: `A`, `B`, …
fn param(i: u32) -> String {
    char::from(b'A' + i as u8).to_string()
}

/// The type `t` as the table writes it (velt_common `std_hooks.rs` module docs).
fn ty(p: &Program, t: TyId) -> String {
    match p.types.kind(t) {
        TyKind::Int(i) => match i {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::ISize => "isize",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::USize => "usize",
        }
        .to_string(),
        TyKind::Float(FloatTy::F32) => "f32".to_string(),
        TyKind::Float(FloatTy::F64) => "f64".to_string(),
        TyKind::Bool => "bool".to_string(),
        TyKind::Str => "string".to_string(),
        TyKind::Unit => "void".to_string(),
        TyKind::Never => "never".to_string(),
        TyKind::Symbol => "symbol".to_string(),
        TyKind::Param(i) => param(*i),
        TyKind::Array(e) => format!("{}[]", ty(p, *e)),
        TyKind::Map(k, v) => format!("Map<{}, {}>", ty(p, *k), ty(p, *v)),
        TyKind::Tuple(xs) => format!("[{}]", list(p, xs)),
        TyKind::Option(x, n) => {
            let none = match n {
                Nothing::Null => "null",
                _ => "undefined",
            };
            format!("{} | {none}", ty(p, *x))
        }
        TyKind::Result(x, e) => format!("{} throws {}", ty(p, *x), ty(p, *e)),
        TyKind::Promise(x, e) => format!("Promise<{}, {}>", ty(p, *x), ty(p, *e)),
        TyKind::Shared(x) => format!("Shared<{}>", ty(p, *x)),
        TyKind::FnPtr {
            params,
            ret,
            throws,
            ..
        } => {
            let throws = match p.types.kind(*throws) {
                TyKind::Never => String::new(),
                _ => format!(" throws {}", ty(p, *throws)),
            };
            format!("({}) => {}{throws}", list(p, params), ty(p, *ret))
        }
        TyKind::Adt(d, args) | TyKind::Dyn(d, args) => {
            let name = match p.def(*d) {
                Def::Adt(a) => &a.name,
                Def::Enum(e) => &e.name,
                Def::Interface(i) => &i.name,
                _ => "?",
            };
            let name = name.rsplit("::").next().unwrap_or(name);
            match args.is_empty() {
                true => name.to_string(),
                false => format!("{name}<{}>", list(p, args)),
            }
        }
        other => format!("{other:?}"),
    }
}

fn list(p: &Program, ts: &[TyId]) -> String {
    ts.iter().map(|t| ty(p, *t)).collect::<Vec<_>>().join(", ")
}

/// The signature of the definition named `qualified`, as the table writes it, if it exists.
fn signature(p: &Program, qualified: &str) -> Option<String> {
    p.defs.iter().find_map(|d| match d {
        Def::Fn(f) if f.name == qualified => {
            let generics = match f.generics {
                0 => String::new(),
                n => format!("<{}>", (0..n).map(param).collect::<Vec<_>>().join(", ")),
            };
            let params: Vec<TyId> = f.params.iter().map(|x| x.ty).collect();
            let throws = match f.throws.map(|t| p.types.kind(t)) {
                None | Some(TyKind::Never) => String::new(),
                Some(_) => format!(" throws {}", ty(p, f.throws.expect("ICE: checked"))),
            };
            Some(format!(
                "{generics}({}) -> {}{throws}",
                list(p, &params),
                ty(p, f.ret)
            ))
        }
        Def::Adt(a) if a.name == qualified => Some("class".to_string()),
        _ => None,
    })
}

#[test]
fn every_hook_resolves_with_its_signature() {
    let p = ok_src(LOADS_ALL);
    let mut wrong = vec![];
    for h in HOOKS {
        let q = h.qualified();
        match signature(&p, &q) {
            Some(sig) if sig == h.sig => {}
            Some(sig) => wrong.push(format!(
                "{}: `{q}` is `{sig}`, the table says `{}`",
                h.name, h.sig
            )),
            None => wrong.push(format!("{}: no definition `{q}`", h.name)),
        }
    }
    assert!(
        wrong.is_empty(),
        "velt_common std_hooks.rs:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn no_program_can_name_a_hook() {
    let mut names: Vec<&str> = HOOKS.iter().map(|h| h.item).collect();
    names.sort();
    names.dedup();
    let uses: String = names
        .iter()
        .map(|n| format!("  const _{n} = {n};\n"))
        .collect();
    let err = err_src(&format!("function main() {{\n{uses}}}\n"));
    for n in names {
        assert!(
            err.contains(&format!("cannot find `{n}`")),
            "a program can name the hook `{n}`:\n{err}"
        );
    }
}

/// The hooks were prelude exports named `__…` (#1946): a program could call them.
#[test]
fn a_program_cannot_call_a_hook_by_its_old_name_or_its_new_one() {
    let err =
        err_src("function main() {\n  console.log(__f64ToString(255, 16), __anyText(42));\n}\n");
    assert!(err.contains("cannot find `__f64ToString`"), "{err}");
    assert!(err.contains("cannot find `__anyText`"), "{err}");
    let err = err_src("function main() {\n  console.log(f64ToString(255, 16));\n}\n");
    assert!(err.contains("cannot find `f64ToString`"), "{err}");
    // What the hook implements still works.
    ok_src("function main() {\n  console.log((255).toString(16));\n}\n");
}

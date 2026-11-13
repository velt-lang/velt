//! The compiler's hooks (velt_common `std_hooks.rs`, docs/internals/design/std-layout.md §10b):
//! every entry of the table names a definition of std with the signature the table gives
//! (compared structurally: velt_common `hook_sig.rs`), and no program can name one.
//!
//! The signatures are compared on a program checked with every std body (`velt check --std`,
//! `CheckOptions::eager_std`): what a hook throws and how it takes each parameter are inferred
//! from its body, which lazy std leaves unchecked for a hook no live code reaches.

mod common;

use common::programs::{err_src, load_src, ok_src, Loaded};
use velt_common::std_hooks::{Mode, Sig, HOOKS, T};
use velt_sema::hir::{Def, DefId, FloatTy, IntTy, Nothing, PassMode, Program, TyId, TyKind};

/// A program that loads every module defining a hook: the prelude loads `velt/lang/hooks`, and
/// the others come with what they implement (`Proxy`, `Set`, `Uint8Array`, `RegExp`, `JSON`; the globals
/// velt/lang/this_fn, velt/lang/record/reflect, velt/lang/hooks/any_record, tuple_elements and
/// velt/lang/hooks/number_float, which the driver's loader adds for what a module uses, and this
/// test's loader does not). velt/lang/hooks/number_text comes with the prelude; imported here,
/// its float hooks, which are checked on demand, are checked too. velt/lang/hooks/sched (the
/// executor in Velt) and velt/lang/hooks/sched_blocking (its pool) are loaded only by a build with
/// `VELT_SCHEDULER=std`, and its debug cell check (velt/lang/hooks/sched_cells) only by a debug
/// compiler's.
const LOADS_ALL: &str = "import \"velt/lang/this_fn\";\nimport \"velt/lang/record/reflect\";\n\
    import \"velt/lang/hooks/any_record\";\nimport \"velt/lang/hooks/number_text\";\n\
    import \"velt/lang/hooks/number_float\";\nimport \"velt/host\";\n\
    import \"velt/lang/hooks/tuple_elements\";\n\
    import \"velt/test/registry\";\nimport \"velt/test/run\";\nimport \"velt/host/promise\";\n\
    import \"velt/ffi/ptr\";\nimport \"velt/ffi/str_bytes\";\nimport \"velt/ffi/c_string\";\n\
    import \"velt/ffi/export\";\n\
    import \"velt/lang/hooks/locale_compare\";\nimport \"velt/lang/hooks/sched\";\n\
    import \"velt/lang/hooks/sched_blocking\";\n\
    import \"velt/lang/hooks/sched_cells\";\n\
    function main() {\n  const p = new Proxy({}, {});\n  const s = new Set<number>();\n  \
    const b = new Uint8Array(1);\n  const r = new RegExp(\"a\");\n  \
    const j = JSON.parse<i64>(\"1\");\n  const v: JsonValue = JsonValue.of(1);\n}\n";

/// Check `l` with every std body (`velt check --std`), requiring success.
fn check_eager(l: &Loaded) -> Program {
    let opts = velt_sema::CheckOptions {
        eager_std: true,
        ..Default::default()
    };
    let (p, d) = velt_sema::check_with(&l.modules, l.root, opts);
    assert!(
        p.is_some() && d.iter().all(|d| !d.is_error()),
        "unexpected diagnostics:\n{}",
        l.render(&d)
    );
    let p = p.unwrap();
    assert!(
        p.unchecked.is_empty(),
        "eager std left definitions unchecked"
    );
    p
}

/// Replace loaded std module `module` (`std/lang/hooks/process`) by `edit` of its source.
fn edit_module(l: &mut Loaded, module: &str, edit: impl FnOnce(&str) -> String) {
    let m = (l.modules.iter().position(|m| m.path == module))
        .unwrap_or_else(|| panic!("`{module}` is not loaded"));
    let src = edit(&l.sm.get(l.modules[m].file).src);
    let id = l.sm.add(format!("{module}.velt"), src.clone());
    let (ast, diags) = velt_syntax::parse_file(id, &src);
    assert!(diags.is_empty(), "the edit of `{module}` does not parse");
    l.modules[m].file = id;
    l.modules[m].ast = ast;
}

/// The type arguments a generic union's payloads are read under: its payload `Param(i)` is
/// `args[i]`, itself read under `outer` (sema renumbers a union definition's parameters,
/// `unions.rs`: the `B | string` of a hook is the definition `T0 | string` applied to `[B]`).
struct Env<'a> {
    args: &'a [TyId],
    outer: Option<&'a Env<'a>>,
}

/// `t` with the type parameters of the unions around it replaced by their arguments.
fn resolve<'a>(
    p: &'a Program,
    mut t: TyId,
    mut env: Option<&'a Env<'a>>,
) -> (TyId, Option<&'a Env<'a>>) {
    while let (TyKind::Param(i), Some(e)) = (p.types.kind(t), env) {
        let Some(&a) = e.args.get(*i as usize) else {
            break;
        };
        (t, env) = (a, e.outer);
    }
    (t, env)
}

/// A union's members: each variant's one payload (`None` when `d` is not a union).
fn union_members(p: &Program, d: DefId) -> Option<Vec<TyId>> {
    match p.def(d) {
        Def::Enum(e) if e.is_union => (e.variants.iter())
            .map(|v| match v.payload.as_slice() {
                [x] => Some(*x),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// The last segment of the name of class, interface or enum `d`.
fn short_name(p: &Program, d: DefId) -> &str {
    let name = match p.def(d) {
        Def::Adt(a) => a.name.as_str(),
        Def::Enum(e) => e.name.as_str(),
        Def::Interface(i) => i.name.as_str(),
        _ => "",
    };
    name.rsplit("::").next().unwrap_or(name)
}

/// HIR type `t` in the table's notation (`T`'s `Display`); a type the notation has no spelling
/// for in Velt's.
fn show(p: &Program, t: TyId, env: Option<&Env>) -> String {
    let (t, env) = resolve(p, t, env);
    let list = |ts: &[TyId], env: Option<&Env>| -> String {
        ts.iter()
            .map(|t| show(p, *t, env))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match p.types.kind(t) {
        TyKind::Int(i) => format!("{i:?}").to_lowercase(),
        TyKind::Float(f) => format!("{f:?}").to_lowercase(),
        TyKind::Bool => "bool".into(),
        TyKind::Str => "string".into(),
        TyKind::Unit => "void".into(),
        TyKind::Never => "never".into(),
        TyKind::Symbol => "symbol".into(),
        TyKind::Error => "<error>".into(),
        TyKind::Param(i) => T::Param(*i).to_string(),
        TyKind::Array(e) => format!("[{}]", show(p, *e, env)),
        TyKind::Map(k, v) => format!("Map({}, {})", show(p, *k, env), show(p, *v, env)),
        TyKind::Tuple(xs) => format!("tuple({})", list(xs, env)),
        TyKind::Option(x, Nothing::Null) => format!("or_null({})", show(p, *x, env)),
        TyKind::Option(x, Nothing::Undefined) => format!("or_undefined({})", show(p, *x, env)),
        TyKind::Option(x, Nothing::Both) => format!("{} | null | undefined", show(p, *x, env)),
        TyKind::Result(x, e) => format!("Result<{}, {}>", show(p, *x, env), show(p, *e, env)),
        TyKind::Promise(x, e) => format!("Promise({}, {})", show(p, *x, env), show(p, *e, env)),
        TyKind::Shared(x) => format!("Shared<{}>", show(p, *x, env)),
        TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } => {
            let kind = if *rest { "FnRest" } else { "Fn" };
            let mut s = format!("{kind}(({}) -> {}", list(params, env), show(p, *ret, env));
            if !matches!(p.types.kind(*throws), TyKind::Never) {
                s += &format!(" throws {}", show(p, *throws, env));
            }
            s + ")"
        }
        TyKind::Closure(_) => "a closure".into(),
        TyKind::Adt(d, args) => match union_members(p, *d) {
            Some(ms) => {
                let inner = Env { args, outer: env };
                format!("union({})", list(&ms, Some(&inner)))
            }
            None => named(p, *d, args, env),
        },
        TyKind::Dyn(d, args) => named(p, *d, args, env),
        TyKind::Literal(v) => format!("the literal type {v:?}"),
        TyKind::Simd(s) => format!("{s:?}").to_lowercase(),
        TyKind::Op(op, args) => format!("{op:?}<{}>", list(args, env)),
    }
}

fn named(p: &Program, d: DefId, args: &[TyId], env: Option<&Env>) -> String {
    let n = short_name(p, d);
    match args {
        [] if n.starts_with('{') => format!("object({n:?})"),
        [] => n.to_string(),
        _ => {
            let args: Vec<String> = args.iter().map(|t| show(p, *t, env)).collect();
            format!("{n}({})", args.join(", "))
        }
    }
}

/// Does HIR type `t` (read under `env`) match the table's `want`? The innermost mismatch is
/// described in `why`.
fn same(p: &Program, t: TyId, env: Option<&Env>, want: &T, why: &mut String) -> bool {
    let (t, env) = resolve(p, t, env);
    let k = p.types.kind(t);
    let ok = match (want, k) {
        (T::I8, TyKind::Int(IntTy::I8))
        | (T::I16, TyKind::Int(IntTy::I16))
        | (T::I32, TyKind::Int(IntTy::I32))
        | (T::I64, TyKind::Int(IntTy::I64))
        | (T::ISize, TyKind::Int(IntTy::ISize))
        | (T::U8, TyKind::Int(IntTy::U8))
        | (T::U16, TyKind::Int(IntTy::U16))
        | (T::U32, TyKind::Int(IntTy::U32))
        | (T::U64, TyKind::Int(IntTy::U64))
        | (T::USize, TyKind::Int(IntTy::USize))
        | (T::F32, TyKind::Float(FloatTy::F32))
        | (T::F64, TyKind::Float(FloatTy::F64))
        | (T::Bool, TyKind::Bool)
        | (T::Str, TyKind::Str)
        | (T::Void, TyKind::Unit)
        | (T::Never, TyKind::Never)
        | (T::Symbol, TyKind::Symbol) => true,
        (T::Param(i), TyKind::Param(j)) => i == j,
        (T::Array(e), TyKind::Array(x)) => same(p, *x, env, e, why),
        (T::Promise(a, b), TyKind::Promise(x, y)) => {
            same(p, *x, env, a, why) && same(p, *y, env, b, why)
        }
        (T::Tuple(ws), TyKind::Tuple(xs)) => all(p, xs, env, ws, why),
        (T::OrNull(w), TyKind::Option(x, Nothing::Null)) => same(p, *x, env, w, why),
        (T::OrUndefined(w), TyKind::Option(x, Nothing::Undefined)) => same(p, *x, env, w, why),
        (
            T::Fn {
                params,
                ret,
                throws,
                rest,
            },
            TyKind::FnPtr {
                params: xs,
                ret: r,
                throws: e,
                rest: rest_,
            },
        ) => {
            let never = T::Never;
            rest == rest_
                && all(p, xs, env, params, why)
                && same(p, *r, env, ret, why)
                && same(p, *e, env, throws.unwrap_or(&never), why)
        }
        (T::Union(ws), TyKind::Adt(d, args)) => match union_members(p, *d) {
            Some(ms) => all(p, &ms, Some(&Env { args, outer: env }), ws, why),
            None => false,
        },
        (T::Named(n, ws), TyKind::Adt(d, args) | TyKind::Dyn(d, args)) => {
            union_members(p, *d).is_none() && short_name(p, *d) == *n && all(p, args, env, ws, why)
        }
        _ => false,
    };
    if !ok && why.is_empty() {
        *why = format!("`{want}` is `{}` there", show(p, t, env));
    }
    ok
}

fn all(p: &Program, ts: &[TyId], env: Option<&Env>, ws: &[T], why: &mut String) -> bool {
    ts.len() == ws.len() && ts.iter().zip(ws).all(|(t, w)| same(p, *t, env, w, why))
}

/// How the definition named `qualified` differs from the table's `sig`: `None` when it matches,
/// what differs otherwise (a missing definition included). `p` is checked whole
/// ([`check_eager`]): every hook's `throws` and parameter modes are inferred from its body.
fn differs(p: &Program, qualified: &str, sig: &Sig) -> Option<String> {
    let found = p.defs.iter().find(|d| match d {
        Def::Fn(f) => f.name == qualified,
        Def::Adt(a) => a.name == qualified,
        Def::ExternFn(x) => x.name == qualified,
        Def::Global(g) => g.name == qualified,
        _ => false,
    });
    let Some(d) = found else {
        return Some("no such definition".to_string());
    };
    let (f, s) = match (d, sig) {
        (Def::Adt(_), Sig::Class) => return None,
        // A `declare decorator function`: what it takes is its own declaration's.
        (Def::ExternFn(x), Sig::Decorator) => {
            return (!x.is_decorator).then(|| "not a `declare decorator function`".to_string())
        }
        (Def::Fn(f), Sig::Fn(s)) => (f, s),
        // A process static: a module constant `new Atomic<u64>(literal)` (no module state slot).
        (Def::Global(g), Sig::ProcessStatic) => {
            let atomic_u64 = matches!(p.types.kind(g.ty), TyKind::Adt(_, args)
                if args.len() == 1 && matches!(p.types.kind(args[0]), TyKind::Int(velt_sema::hir::IntTy::U64)));
            return match (atomic_u64, g.slot) {
                (true, None) => None,
                (false, _) => Some("not an `Atomic<u64>`".to_string()),
                (true, Some(_)) => {
                    Some("kept in the module state, not a static atomic".to_string())
                }
            };
        }
        _ => {
            return Some(
                "a class, function or decorator where the table has another of them".to_string(),
            )
        }
    };
    if f.is_async != s.is_async {
        return Some(format!("async: {}", f.is_async));
    }
    if f.generics != s.generics {
        return Some(format!("{} type parameters", f.generics));
    }
    if f.params.len() != s.params.len() {
        return Some(format!("{} parameters", f.params.len()));
    }
    let mut why = String::new();
    for (n, (x, w)) in f.params.iter().zip(s.params).enumerate() {
        if !same(p, x.ty, None, &w.ty, &mut why) {
            return Some(format!("parameter {}: {why}", n + 1));
        }
    }
    if !same(p, f.ret, None, &s.ret, &mut why) {
        return Some(format!("result: {why}"));
    }
    let throws = f
        .throws
        .filter(|t| !matches!(p.types.kind(*t), TyKind::Never));
    match (throws, &s.throws) {
        (None, None) => {}
        (Some(t), Some(w)) => {
            if !same(p, t, None, w, &mut why) {
                return Some(format!("throws: {why}"));
            }
        }
        (Some(t), None) => {
            return Some(format!("throws `{}`, the table has none", show(p, t, None)))
        }
        (None, Some(w)) => return Some(format!("throws nothing, the table has `{w}`")),
    }
    for (n, (x, w)) in f.params.iter().zip(s.params).enumerate() {
        let ok = matches!(
            (x.mode, w.mode),
            (PassMode::Copy, Mode::Value)
                | (PassMode::Borrow, Mode::Borrow)
                | (PassMode::BorrowMut, Mode::BorrowMut)
                | (PassMode::Owned, Mode::Owned)
        );
        if !ok {
            return Some(format!("parameter {}: passed {:?}", n + 1, x.mode));
        }
    }
    None
}

/// Every entry of the table that differs from `p`'s definition, described.
fn wrong_hooks(p: &Program) -> Vec<String> {
    let mut wrong = vec![];
    for h in HOOKS {
        let q = h.qualified();
        if let Some(why) = differs(p, &q, &h.sig) {
            wrong.push(format!("{}: `{q}` is not `{}`: {why}", h.name, h.sig));
        }
    }
    wrong
}

#[test]
fn every_hook_resolves_with_its_signature() {
    let wrong = wrong_hooks(&check_eager(&load_src(LOADS_ALL)));
    assert!(
        wrong.is_empty(),
        "velt_common std_hooks.rs:\n{}",
        wrong.join("\n")
    );
}

/// Lazy std checks the lowered hooks of the values a program has: lowering calls them
/// (`velt_common` `hook_sig.rs`). This program holds an `any`, arrays and functions, and has no
/// class object, no reference to a host value, no float printed and no `catchUnwind`, which the
/// unchecked hooks wait for (a printed float for both of its writers).
#[test]
fn lazy_std_checks_the_lowered_hooks_of_the_values_a_program_has() {
    let p = ok_src(LOADS_ALL);
    let unchecked: Vec<&str> = (HOOKS.iter().filter(|h| h.lowered))
        .filter(|h| {
            let q = h.qualified();
            let i = (p.defs.iter()).position(|d| matches!(d, Def::Fn(f) if f.name == q));
            i.is_some_and(|i| p.unchecked.contains(&DefId(i as u32)))
        })
        .map(|h| h.name)
        .collect();
    assert_eq!(
        unchecked,
        [
            "number_write",
            "whole_number_write",
            "panic_raise",
            "class_object",
            "host_ref_handle"
        ]
    );
}

/// A hook no code reaches is compared by its body too: lazy std leaves `processWrite` unchecked,
/// and a `throws` there (inferred from its body) differs from its entry.
#[test]
fn a_throws_on_a_hook_no_code_reaches_differs() {
    let mut l = load_src(LOADS_ALL);
    edit_module(&mut l, "std/lang/hooks/process", |src| {
        let old = "s: string): bool {\n";
        assert!(src.contains(old), "processWrite's signature moved");
        src.replace(
            old,
            "s: string): bool throws RangeError {\n  if (stream > 2) {\n    \
             throw new RangeError(\"stream\");\n  }\n",
        )
    });
    let wrong = wrong_hooks(&check_eager(&l));
    assert_eq!(
        wrong,
        ["process_write: `std/lang/hooks/process::processWrite` is not `(u32, &string) -> bool`: \
          throws `RangeError`, the table has none"],
    );
}

#[test]
fn no_program_can_name_a_hook() {
    // `export` (velt/ffi's decorator) is a keyword, which no expression can name anyway.
    let mut names: Vec<&str> = (HOOKS.iter().map(|h| h.item))
        .filter(|n| *n != "export")
        .collect();
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

/// Each key of the table (std/lang/hooks/keys.velt) names a std method under the member name
/// `std_key!` spells: sema names std's symbols first, so a program's own symbol of the same
/// description cannot take the name.
#[test]
fn every_key_names_a_method_of_std() {
    let src = format!("const MINE = Symbol(\"mapCursor\");\n{LOADS_ALL}");
    let p = ok_src(&src);
    for k in velt_common::std_hooks::KEYS {
        let member = format!(".\u{FFFF}Symbol({})", k.description);
        let found = p.defs.iter().any(|d| match d {
            Def::Fn(f) => f.name.starts_with("std/") && f.name.ends_with(&member),
            _ => false,
        });
        assert!(
            found,
            "no std method keyed by `{}` ({})",
            k.constant, k.name
        );
    }
    assert_eq!(
        velt_common::std_key!(map_cursor),
        velt_sema::property_order::symbol_key_name("mapCursor", 1)
    );
}

/// A generic union is compared with its type arguments: sema's definition of `B | string` is
/// `T0 | string` applied to `[B]`, so the table's `union(B, string)` matches and `union(A, string)`
/// does not (no hook takes a generic union today).
#[test]
fn a_generic_union_is_compared_with_its_type_arguments() {
    let p = ok_src("function f<A, B>(a: A, x: B | string): void {}\nfunction main() {}\n");
    let f = (p.defs.iter())
        .find_map(|d| match d {
            Def::Fn(f) if f.name == "f" => Some(f),
            _ => None,
        })
        .expect("f");
    let x = f.params[1].ty;
    // The order sema lists the members in (its canonical order).
    let b_first = same(
        &p,
        x,
        None,
        &T::Union(&[T::Param(1), T::Str]),
        &mut String::new(),
    );
    let (right, wrong): (&[T], &[T]) = if b_first {
        (&[T::Param(1), T::Str], &[T::Param(0), T::Str])
    } else {
        (&[T::Str, T::Param(1)], &[T::Str, T::Param(0)])
    };
    assert!(same(&p, x, None, &T::Union(right), &mut String::new()));
    let mut why = String::new();
    assert!(!same(&p, x, None, &T::Union(wrong), &mut why));
    let shown = if b_first {
        "union(B, string)"
    } else {
        "union(string, B)"
    };
    assert_eq!(why, "`A` is `B` there");
    assert_eq!(show(&p, x, None), shown);
}

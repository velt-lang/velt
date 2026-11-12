//! A kept check (`velt_sema::kept`, dev-warm-front-end.md step 3 "C2"): the checking context of a
//! check, kept with everything it borrows. It reads its modules through its borrows after the
//! check and the driver's handles are gone, and it takes over only the very modules it borrows.

mod common;

use common::programs::load_src;
use velt_sema::generated::GenInput;
use velt_sema::{check_generating_kept, CheckOptions, Checked, KeptCheck, PendingKept};

/// Check `src` asking for a kept check: the pending kept check, if any, and the modules checked.
fn pending(src: &str) -> (Option<PendingKept>, Vec<velt_sema::SourceModule>) {
    let l = load_src(src);
    let modules = l.modules;
    let opts = CheckOptions::default();
    let input = GenInput::default();
    let ((out, diags), pending) =
        check_generating_kept(&modules, l.root, opts, &input, &[], None, None);
    assert!(
        matches!(out, Some(Checked::Program(_))),
        "the program checks: {diags:?}"
    );
    (pending, modules)
}

/// The kept check of `src`, and what it should read: the paths and the item count of the
/// modules, taken before they were handed over.
fn kept(src: &str) -> (KeptCheck, Vec<String>, usize) {
    let (pending, modules) = pending(src);
    let paths: Vec<String> = modules.iter().map(|m| m.path.clone()).collect();
    let items = modules.iter().map(|m| m.ast.items.len()).sum::<usize>();
    let pending = pending.expect("a kept check");
    // SAFETY: `modules` is the vector the check was given, alive and unchanged since.
    let kept = unsafe { pending.adopt(modules, vec![], None) };
    let kept = kept.ok().expect("the same modules");
    (kept, paths, items)
}

#[test]
fn a_kept_check_reads_its_modules_after_the_check() {
    let (kept, paths, items) = kept(
        "function twice(n: i64): i64 { return n * 2; }
         function main() { console.log(twice(2)); }",
    );
    // Every module the driver gave, then nothing it did not (no std loaded on demand here).
    assert_eq!(kept.module_paths(), paths);
    assert!(kept.items() >= items, "{} < {items}", kept.items());
    assert!(kept.definitions() > 0);
    assert!(!kept.owns_rewritten_modules());
    assert!(kept.borrows_only_what_it_owns());
}

#[test]
fn a_kept_check_owns_the_rewritten_modules_it_borrows() {
    // A closure nested in a closure and an overload make the source rewrites copy the modules:
    // the context then borrows the copy, which the kept check owns.
    let (kept, paths, _) = kept(
        "function pick(x: number): number;
         function pick(x: string): string;
         function pick(x: number | string): number | string { return x; }
         function main() {
           const outer = (n: number) => { const inner = (k: number) => k + n; return inner(1); };
           console.log(outer(2), pick(\"a\"));
         }",
    );
    assert!(kept.owns_rewritten_modules());
    assert!(kept.borrows_only_what_it_owns());
    let read = kept.module_paths().len();
    assert_eq!(read, paths.len());
    assert!(kept.items() > 0);
}

#[test]
fn a_kept_check_takes_over_only_the_modules_it_borrows() {
    let (pending, modules) = pending("function main() { console.log(1); }");
    let pending = pending.expect("a kept check");
    // The same program loaded again, at another address, is refused and comes back.
    let copy = load_src("function main() { console.log(1); }").modules;
    // SAFETY: refused before anything reads through it (another vector, at another address,
    // while the one checked is alive).
    let Err(back) = (unsafe { pending.adopt(copy, vec![], None) }) else {
        panic!("a copy of the modules was taken over");
    };
    assert_eq!(back.input.len(), modules.len());
}

#[test]
fn a_check_not_asked_to_keep_keeps_nothing() {
    let l = load_src("function main() { console.log(1); }");
    let (out, _) = velt_sema::check_generating(
        &l.modules,
        l.root,
        CheckOptions::default(),
        &GenInput::default(),
        &[],
        None,
        None,
    );
    assert!(matches!(out, Some(Checked::Program(_))));
    // The request is per check: a later check that asks gets its own.
    let (pending, _) = pending("function main() { console.log(2); }");
    assert!(pending.is_some());
}

/// A program with the constructs a body's check records: closures (nested, and a callback),
/// inferred results, a class with methods, a generic function, a `Map`, an error thrown and
/// caught, template strings.
const BODIES: &str = "
class Counter {
  n: number;
  constructor() { this.n = 0; }
  bump(by: number) { this.n += by; return this.n; }
}
function twice<T>(x: T): T[] { return [x, x]; }
function parse(text: string): number {
  const n = Number(text);
  if (Number.isNaN(n)) { throw new Error(`bad ${text}`); }
  return n;
}
function total(rows: string[]) {
  const m = new Map<string, number>();
  let sum = 0;
  rows.forEach((r) => {
    const add = (k: number) => { sum += k; return sum; };
    m.set(r, add(parse(r)));
  });
  return sum + m.size;
}
function main() {
  const c = new Counter();
  c.bump(2);
  try { console.log(total([\"1\", \"2\"]), twice(\"a\").length, c.n); }
  catch (e) { console.log(\"failed\"); }
}
";

/// `p` printed with every hash map and set as its sorted entries (their order is a hash's).
fn canonical(mut p: Box<velt_sema::hir::Program>) -> String {
    let mut maps = vec![];
    macro_rules! sorted {
        ($($f:ident),*) => {$(
            let mut v: Vec<String> = p.$f.iter().map(|e| format!("{e:?}")).collect();
            v.sort();
            maps.push(format!("{}: {v:#?}", stringify!($f)));
            p.$f.clear();
        )*};
    }
    sorted!(
        anon_shapes,
        union_shapes,
        fn_shapes,
        type_ops,
        super_calls,
        upcasts,
        upcast_impls,
        cpu_targets,
        type_names,
        generic_dyn_methods,
        arg_free_generic_fns,
        fresh_closures,
        fn_wrappers,
        extension_impls,
        dyn_bounds,
        lock_free_callbacks,
        unchecked,
        generated,
        decorators
    );
    for (name, set) in [
        (
            "dyn_members",
            p.dyn_members
                .take()
                .map(|m| m.into_iter().map(|e| format!("{e:?}")).collect::<Vec<_>>()),
        ),
        (
            "live",
            p.live
                .take()
                .map(|m| m.into_iter().map(|e| format!("{e:?}")).collect::<Vec<_>>()),
        ),
    ] {
        let mut v = set.unwrap_or_default();
        v.sort();
        maps.push(format!("{name}: {v:#?}"));
    }
    format!("{p:#?}\n{}", maps.join("\n"))
}

/// The checked program of `src` and its kept check.
fn program_and_kept(src: &str) -> (Box<velt_sema::hir::Program>, KeptCheck) {
    let l = load_src(src);
    let modules = l.modules;
    let input = GenInput::default();
    let ((out, diags), pending) = check_generating_kept(
        &modules,
        l.root,
        CheckOptions::default(),
        &input,
        &[],
        None,
        None,
    );
    let Some(Checked::Program(program)) = out else {
        panic!("the program checks: {diags:?}");
    };
    let pending = pending.expect("a kept check");
    // SAFETY: `modules` is the vector the check was given, alive and unchanged since.
    let kept = unsafe { pending.adopt(modules, vec![], None) };
    (program, kept.ok().expect("the same modules"))
}

#[test]
fn a_reload_of_unchanged_bodies_gives_the_program_the_check_gave() {
    let (cold, kept) = program_and_kept(BODIES);
    let names = kept.own_functions();
    let warm = kept
        .reload_unchanged(&names, CheckOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let (a, b) = (canonical(cold), canonical(warm));
    if a != b {
        let line = (a.lines().zip(b.lines()))
            .position(|(x, y)| x != y)
            .unwrap_or(0);
        let show = |t: &str| {
            t.lines()
                .skip(line.saturating_sub(3))
                .take(30)
                .collect::<Vec<_>>()
                .join("\n")
        };
        std::fs::write(std::env::temp_dir().join("cold.txt"), &a).ok();
        std::fs::write(std::env::temp_dir().join("warm.txt"), &b).ok();
        panic!(
            "the reload differs at line {line}:\ncold:\n{}\nwarm:\n{}",
            show(&a),
            show(&b)
        );
    }
}

/// The cold check of `edited` against a warm reload of the kept check of `BODIES` with
/// `functions` checked again from `edited`'s modules: equal programs.
fn assert_warm_reload_matches(edited: &str, functions: &[&str]) {
    assert_warm_reload_matches_spans(edited, functions, true);
}

/// [`assert_warm_reload_matches`]; `spans`: compare source positions too (an edit that keeps
/// the text's length), else leave them out (positions after a longer or shorter edit move in
/// the cold check, not in the kept one: lowering shifts them).
fn assert_warm_reload_matches_spans(edited: &str, functions: &[&str], spans: bool) {
    let (_, kept) = program_and_kept(BODIES);
    let (cold, _) = program_and_kept(edited);
    let l = load_src(edited);
    let names: Vec<String> = functions.iter().map(|s| s.to_string()).collect();
    let warm = kept
        .reload(&l.modules, l.root, &names, CheckOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    // A closure the edit adds, and the types only it uses, get other ids in the reload (made
    // last) than in the cold check (made in order): named by what they are.
    let (cold_names, warm_names) = (id_names(&cold), id_names(&warm));
    let (mut cold, mut warm) = (cold, warm);
    // Their definitions too are in another order: compared as a set, by what they say.
    let defs =
        |p: &mut velt_sema::hir::Program, names: &(Vec<String>, Vec<String>)| -> Vec<String> {
            let mut v: Vec<String> = (p.defs.iter())
                .map(|d| without_positions(&with_names(&format!("{d:#?}"), names)))
                .collect();
            v.sort();
            p.defs.clear();
            v
        };
    let (cold_defs, warm_defs) = match spans {
        true => (vec![], vec![]),
        false => (defs(&mut cold, &cold_names), defs(&mut warm, &warm_names)),
    };
    if cold_defs != warm_defs {
        let at = (cold_defs.iter().zip(&warm_defs))
            .position(|(x, y)| x != y)
            .unwrap_or(0);
        let (x, y) = (cold_defs.get(at), warm_defs.get(at));
        panic!(
            "a definition differs:\ncold: {}\nwarm: {}",
            without_positions(x.map_or("", |s| s)),
            without_positions(y.map_or("", |s| s))
        );
    }
    let (mut a, mut b) = (canonical(cold), canonical(warm));
    if !spans {
        a = with_names(&a, &cold_names);
        b = with_names(&b, &warm_names);
    }
    if !spans {
        // The rest (the definitions are compared above): maps keyed by position sort otherwise
        // once positions are left out, so as sorted lines.
        let lines = |t: &str| -> String {
            let mut v: Vec<&str> = t.lines().collect();
            v.sort_unstable();
            v.join("\n")
        };
        (a, b) = (lines(&without_positions(&a)), lines(&without_positions(&b)));
    }
    if a != b {
        let line = (a.lines().zip(b.lines()))
            .position(|(x, y)| x != y)
            .unwrap_or(0);
        let show = |t: &str| {
            t.lines()
                .skip(line.saturating_sub(3))
                .take(20)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (x, y): (
            std::collections::BTreeSet<&str>,
            std::collections::BTreeSet<&str>,
        ) = (a.lines().collect(), b.lines().collect());
        let only_cold: Vec<_> = x.difference(&y).take(5).collect();
        let only_warm: Vec<_> = y.difference(&x).take(5).collect();
        panic!("the reload differs at line {line} ({} against {} lines); only cold: {only_cold:#?}; only warm: {only_warm:#?}\ncold:\n{}\nwarm:\n{}", a.lines().count(), b.lines().count(), show(&a), show(&b));
    }
}

#[test]
fn a_reload_of_an_edited_message_gives_the_edited_program() {
    // The same length: every other span stays where it was.
    assert_warm_reload_matches(&BODIES.replace("bad ${text}", "BAD ${text}"), &["parse"]);
}

#[test]
fn a_reload_of_an_edited_result_gives_the_edited_program() {
    assert_warm_reload_matches(&BODIES.replace("return n;", "return 7;"), &["parse"]);
}

#[test]
fn a_reload_of_an_edit_inside_a_closure_gives_the_edited_program() {
    assert_warm_reload_matches(&BODIES.replace("sum += k;", "sum -= k;"), &["total"]);
}

#[test]
fn a_reload_of_an_edit_adding_a_closure_gives_the_edited_program() {
    let edited = BODIES.replace(
        "return sum + m.size;",
        "const f = (x: number) => x * 2; return f(sum) + m.size;",
    );
    assert_warm_reload_matches_spans(&edited, &["total"], false);
}

#[test]
fn a_reload_of_a_longer_edit_gives_the_edited_program_but_its_positions() {
    let edited = BODIES.replace("bad ${text}", "not a number: ${text}");
    assert_warm_reload_matches_spans(&edited, &["parse"], false);
}

/// What each definition and type id of `p` stands for: a definition by its name, a type by its
/// kind, with the ids inside named in turn.
fn id_names(p: &velt_sema::hir::Program) -> (Vec<String>, Vec<String>) {
    let defs: Vec<String> = (0..p.defs.len())
        .map(|i| match p.def(velt_sema::hir::DefId(i as u32)) {
            velt_sema::hir::Def::Fn(f) => format!("fn {}", f.name),
            other => format!("{other:?}").chars().take(60).collect(),
        })
        .collect();
    let mut tys: Vec<String> = vec![];
    for i in 0..p.types.len() {
        let kind = format!("{:?}", p.types.kind(velt_sema::hir::TyId(i as u32)));
        tys.push(replace_ids(&kind, "TyId(", &|n| {
            tys.get(n).cloned().unwrap_or_else(|| format!("ty{n}"))
        }));
    }
    let tys = tys
        .iter()
        .map(|t| replace_ids(t, "DefId(", &|n| defs.get(n).cloned().unwrap_or_default()))
        .collect();
    (defs, tys)
}

/// `text` with every `DefId(n)` and `TyId(n)` replaced by what it stands for (`id_names`).
fn with_names(text: &str, (defs, tys): &(Vec<String>, Vec<String>)) -> String {
    let t = replace_ids(text, "DefId(", &|n| {
        defs.get(n).cloned().unwrap_or_default()
    });
    replace_ids(&t, "TyId(", &|n| tys.get(n).cloned().unwrap_or_default())
}

/// `text` with each `<open>n)` (whitespace allowed around `n`, as `{:#?}` prints it) replaced
/// by `name(n)`.
fn replace_ids(text: &str, open: &str, name: &dyn Fn(usize) -> String) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(open) {
        out.push_str(&rest[..at]);
        let after = &rest[at + open.len()..];
        let digits: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let close = after.find(')');
        match (digits.parse::<usize>(), close) {
            (Ok(n), Some(c)) => {
                out.push('<');
                out.push_str(&name(n));
                out.push('>');
                rest = &after[c + 1..];
            }
            _ => {
                out.push_str(open);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `text` without source positions (`lo: n, hi: m`, on one line or as `{:#?}` prints them).
fn without_positions(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("lo: ") && !l.trim_start().starts_with("hi: "))
        .collect();
    let mut out = String::new();
    for l in lines {
        let mut l = l.to_string();
        while let Some(at) = l.find("lo: ") {
            let end = l[at..].find('}').map(|e| at + e).unwrap_or(l.len());
            l.replace_range(at..end, "");
        }
        out.push_str(&l);
        out.push('\n');
    }
    out
}

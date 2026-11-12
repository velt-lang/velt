//! Swap or restart, from real sources through the whole pipeline (front end, lowering, JIT):
//! each case compiles a first version, loads it, then reloads a second one without running
//! either.

use std::path::PathBuf;

use velt_codegen_cl::{DevSession, Reload};

use crate::driver::{self, BuildOptions, Session};

fn compile(source: &str) -> velt_vir::vir::Program {
    let opts = BuildOptions {
        input: PathBuf::from("main.velt"),
        root_source: Some(source.to_string()),
        ..BuildOptions::default()
    };
    let mut sess = Session::new();
    driver::compile(&mut sess, &opts)
        .unwrap_or_else(|e| panic!("{e:?}\n{}", sess.render_diagnostics()))
}

/// Load `before`, reload `after`.
fn reload(before: &str, after: &str) -> Reload {
    let mut session = DevSession::new(&velt_rt_host::abi_symbols::symbol_table());
    session.load(&compile(before)).expect("load");
    session.reload(&compile(after)).expect("reload")
}

/// A program around `items` (top-level declarations) whose `main` runs `body`.
fn program(items: &str, body: &str) -> String {
    format!("{items}\nasync function main() {{\n{body}\n}}\n")
}

fn swaps(before: &str, after: &str) -> usize {
    match reload(before, after) {
        Reload::Swapped { functions } => functions,
        Reload::Restart(reason) => panic!("restarted ({reason})"),
    }
}

fn restarts(before: &str, after: &str) -> String {
    match reload(before, after) {
        Reload::Restart(reason) => reason,
        other => panic!("{other:?}"),
    }
}

const MAIN: &str = "  console.log(label(2));\n  await tick();";

#[test]
fn body_edits_swap_only_what_changed() {
    let items = |text: &str| {
        format!(
            "function label(n: number): string {{ return `{text} ${{n}}`; }}\n\
             async function tick() {{ await sleep(1); }}"
        )
    };
    let before = program(&items("n ="), MAIN);
    assert_eq!(swaps(&before, &before), 0);
    assert_eq!(swaps(&before, &program(&items("n is"), MAIN)), 1);
}

#[test]
fn an_async_body_edit_swaps_with_its_state_machine() {
    let items = |extra: &str| {
        format!(
            "function label(n: number): string {{ return `${{n}}`; }}\n\
             async function tick() {{ const a = 1; await sleep(1); {extra} }}\n\
             async function tock() {{ await tick(); }}"
        )
    };
    let main = "  console.log(label(2));\n  await tock();";
    let before = program(&items(""), main);
    let after = program(&items("console.log(`${a}`); await sleep(2);"), main);
    // `tick`'s poll and drop, and the state machines embedding its state (`tock`, `main`).
    assert!(swaps(&before, &after) >= 4);
}

#[test]
fn generic_instances_swap() {
    let items = |open: &str| {
        format!(
            "function wrap<T>(x: T): string {{ return `{open}${{x}}`; }}\n\
             function label(n: number): string {{ return wrap(n); }}\n\
             async function tick() {{ await sleep(1); }}"
        )
    };
    assert_eq!(
        swaps(&program(&items("["), MAIN), &program(&items("<"), MAIN)),
        1
    );
}

#[test]
fn layout_and_signature_changes_restart() {
    let point = |fields: &str| {
        format!(
            "class Point {{ {fields} constructor(x: number) {{ this.x = x; }} }}\n\
             function label(n: number): string {{ return `${{new Point(n).x}}`; }}\n\
             async function tick() {{ await sleep(1); }}"
        )
    };
    let before = program(&point("x: number;"), MAIN);
    let after = program(&point("x: number; y: number = 0;"), MAIN);
    assert_eq!(restarts(&before, &after), "Point gained a field");

    let sig = |params: &str, call: &str| {
        program(
            &format!(
                "function label({params}): string {{ return `x`; }}\n\
                 async function tick() {{ await sleep(1); }}"
            ),
            &format!("  console.log({call});\n  await tick();"),
        )
    };
    let reason = restarts(
        &sig("n: number", "label(2)"),
        &sig("n: number, m: number", "label(2, 3)"),
    );
    assert_eq!(reason, "the signature of label changed");
}

#[test]
fn main_and_closure_capture_changes_restart() {
    let items = "function label(n: number): string { return `${n}`; }\n\
                 async function tick() { await sleep(1); }";
    let before = program(items, MAIN);
    let after = program(items, "  console.log(label(3));\n  await tick();");
    assert_eq!(restarts(&before, &after), "main changed (it already ran)");

    let captures = |extra: &str, used: &str| {
        program(
            "",
            &format!(
                "  const a = 1;\n  {extra}\n  const f = () => {used};\n  console.log(`${{f()}}`);\n  await sleep(1);"
            ),
        )
    };
    let reason = restarts(&captures("", "a"), &captures("const b = 2;", "a + b"));
    assert_eq!(reason, "the captures of main::{closure#0} changed");
}

#[test]
fn module_state_edits_swap_unless_the_layout_changes() {
    // `a` is written (`grow`), so it is a slot of the module state, not a constant.
    let items = |lets: &str, slot: &str, show: &str| {
        format!(
            "{lets}
             function grow(): void {{ {slot} = {slot} + {slot}; }}
             function label(n: number): string {{ grow(); return `${{n}} ${{{show}}}`; }}
             async function tick() {{ await sleep(1); }}"
        )
    };
    let before = program(&items("let a = 1;", "a", "a"), MAIN);
    // A body edit keeps the key: the block is shared, only `label` is compiled.
    assert_eq!(
        swaps(&before, &program(&items("let a = 1;", "a", "a + 1"), MAIN)),
        1
    );
    // An initializer edit keeps the layout: a block of its own, every user recompiled.
    assert!(swaps(&before, &program(&items("let a = 2;", "a", "a"), MAIN)) >= 3);
    // A rename changes the descriptor's slot names, which `main` reaches.
    let renamed = program(&items("let b = 1;", "b", "b"), MAIN);
    assert_eq!(restarts(&before, &renamed), "main changed (it already ran)");
    let more = items("let a = 1; let b = 2;", "a", "a + b");
    assert_eq!(
        restarts(&before, &program(&more, MAIN)),
        "module state gained a slot"
    );
    // An initializer that builds a value (an array) is no layout change either.
    let list = |init: &str| {
        program(
            &format!(
                "let s: string[] = {init};
                 function label(n: number): string {{ s.push(\"a\"); return `${{n}} ${{s.length}}`; }}
                 async function tick() {{ await sleep(1); }}"
            ),
            MAIN,
        )
    };
    assert!(swaps(&list("[]"), &list("[\"x\"]")) >= 2);
    let text = items("let a = \"1\";", "a", "a");
    let reason = restarts(&before, &program(&text, MAIN));
    assert_eq!(reason, "the type of a module state slot changed");
}

/// `Node` is not counted: each one is handed over to the array.
const UNCOUNTED: &str = "class Node { v: number = 0; }
let saved: Node[] = [];
function put(n: Node) { saved.push(n); }
function make(): number { const a = new Node(); put(a); return saved.length; }
async function main() { console.log(make()); await sleep(1); }
";

/// [`UNCOUNTED`] after an edit that keeps using a `Node` after storing it twice: counted.
const COUNTED: &str = "class Node { v: number = 0; }
let saved: Node[] = [];
function put(n: Node) { saved.push(n); }
function make(): number { const a = new Node(); put(a); put(a); a.v += 1; return saved.length; }
async function main() { console.log(make()); await sleep(1); }
";

/// The host's reload of `source`, with `load` standing for what it loads after the build.
fn host_reload(
    session: &mut DevSession,
    carried: &mut super::Carried,
    source: &str,
    load: Result<(), String>,
) -> velt_rt_host::dev::handover::reload::Reloaded {
    let opts = BuildOptions {
        input: PathBuf::from("main.velt"),
        root_source: Some(source.to_string()),
        ..BuildOptions::default()
    };
    let flags = super::Report {
        verbose: false,
        details: false,
    };
    super::reload_with(session, &opts, flags, carried, |_| load)
}

/// A version that fails after lowering (a C library that does not load) never ran: the next
/// version is compared with the running one, not with it (#2653). Here the failed version
/// starts counting `Node`, and the fixed one that follows must restart.
#[test]
fn a_version_that_failed_after_lowering_is_not_the_baseline() {
    use velt_rt_host::dev::handover::reload::Reloaded;
    let mut sess = Session::new();
    sess.counted_seed = Some(velt_vir::CountedSeed::default());
    let opts = BuildOptions {
        input: PathBuf::from("main.velt"),
        root_source: Some(UNCOUNTED.to_string()),
        ..BuildOptions::default()
    };
    let first = driver::compile(&mut sess, &opts).expect("v1");
    let mut session = DevSession::new(&velt_rt_host::abi_symbols::symbol_table());
    session.load(&first).expect("load");
    let mut carried = super::Carried {
        seed: sess.counted_seed.take(),
        own: None,
        interfaces: None,
        kept: None,
        kept_own: None,
        kept_files: None,
        warm: true,
        warm_reloads: 0,
        kept_lowering: None,
        dev_program: None,
    };
    let failed = host_reload(
        &mut session,
        &mut carried,
        COUNTED,
        Err("no library".into()),
    );
    assert!(matches!(failed, Reloaded::Failed { .. }), "{failed:?}");
    match host_reload(&mut session, &mut carried, COUNTED, Ok(())) {
        Reloaded::Restart { reason, .. } => assert_eq!(reason, "class Node is now counted"),
        other => panic!("{other:?}"),
    }
}

/// A `velt dev` version keeps every glue entry of its tables, also those nothing in it
/// dispatches (velt_vir `glue/demand.rs`, `slot_uses.rs`): a value it made keeps its table, and a
/// later version swapped in may clone, log or send that value through it.
#[test]
fn a_dev_version_keeps_every_glue_entry() {
    const SOURCE: &str = "
interface Shape {
  area(): number;
}
class Animal {
  legs: number[] = [4];
  speak(): number {
    return 1;
  }
}
class Dog extends Animal implements Shape {
  override speak(): number {
    return 2;
  }
  area(): number {
    return this.legs[0];
  }
}
const s: Shape = new Dog();
const a: Animal = new Dog();
console.log(s.area() + a.speak());
";
    let symbols = |seed: Option<velt_vir::CountedSeed>| -> Vec<String> {
        let mut sess = Session::new();
        sess.counted_seed = seed;
        let opts = BuildOptions {
            input: PathBuf::from("main.velt"),
            root_source: Some(SOURCE.to_string()),
            ..BuildOptions::default()
        };
        let p = driver::compile(&mut sess, &opts).expect("compiles");
        p.funcs.iter().map(|f| f.symbol.clone()).collect()
    };
    let has = |syms: &[String], prefix: &str| syms.iter().any(|s| s.starts_with(prefix));
    let glue = [
        "_Gdynclone_3Dog",
        "_Gdynformat_3Dog",
        "_Gdynshare_3Dog",
        "_Gdyntransfer_3Dog",
        "_Gobjclone_3Dog",
        "_Gobjtransfer_3Dog",
    ];
    let plain = symbols(None);
    let dev = symbols(Some(velt_vir::CountedSeed::default()));
    for g in glue {
        // Class glue is lowered and dropped later (`dead_funcs`); interface glue is never lowered.
        if g.starts_with("_Gdyn") {
            assert!(!has(&plain, g), "{g} in a plain build");
        }
        assert!(has(&dev, g), "{g} missing from a dev version");
    }
}

#[test]
fn a_dev_build_keeps_its_check_when_asked() {
    let source = program(
        "function twice(n: i64): i64 { return n * 2; }",
        "console.log(twice(2));",
    );
    let opts = BuildOptions {
        input: PathBuf::from("main.velt"),
        root_source: Some(source),
        ..BuildOptions::default()
    };
    let mut sess = Session::new();
    sess.keep_check = true;
    driver::compile(&mut sess, &opts).unwrap_or_else(|e| panic!("{e:?}"));
    // It owns the program's modules: reading their paths goes through the kept context.
    let kept = sess.kept.take().expect("a kept check");
    assert!(kept.module_paths().iter().any(|p| p.contains("main")));
    assert!(kept.definitions() > 0 && kept.items() > 0);
    assert!(kept.borrows_only_what_it_owns());
    // Not asked, nothing is kept.
    let mut sess = Session::new();
    driver::compile(&mut sess, &opts).unwrap_or_else(|e| panic!("{e:?}"));
    assert!(sess.kept.is_none());
}

/// A version built cold and kept, then a reload of `edited` from its kept check: the program,
/// or why the cold front end was needed. `files` are the program's (relative path, text), the
/// root `main.velt` first; `edited` replaces some of them.
fn warm_reload(
    files: &[(&str, &str)],
    edited: &[(&str, &str)],
) -> (Result<String, String>, String) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let write = |files: &[(&str, &str)]| {
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
            std::fs::write(path, text).expect("written");
        }
    };
    write(files);
    let opts = BuildOptions {
        input: dir.path().join("main.velt"),
        ..BuildOptions::default()
    };
    // A check in which a generator ran is not kept: the next build uses its code from the cache.
    let cold_build = || {
        let mut sess = Session::new();
        sess.keep_check = true;
        sess.keep_lowering = true;
        sess.counted_seed = Some(velt_vir::CountedSeed::default());
        let program = driver::compile_for_jit(&mut sess, &opts).unwrap_or_else(|e| panic!("{e:?}"));
        sess.dev_program = Some(program);
        sess
    };
    let mut sess = cold_build();
    if sess.kept.is_none() {
        sess = cold_build();
    }
    assert!(sess.kept.is_some(), "a dev build keeps its check");
    assert!(
        sess.kept_lowering.is_some(),
        "a dev build keeps its lowering"
    );
    let base = super::super::edits::Snapshot::of_build(&sess, &opts.input);
    let cold_files = driver::copy_files(&sess.sm);
    let mut warm = Session::new();
    warm.kept = sess.kept.take();
    warm.kept_lowering = sess.kept_lowering.take();
    warm.dev_program = sess.dev_program.take();
    warm.debug_kept = sess.debug_kept.take();
    assert!(
        warm.debug_kept.is_some(),
        "a dev build keeps its optimizer's facts"
    );
    warm.counted_seed = sess.counted_seed.take();
    write(edited);
    let input = opts.input.clone();
    let located = move |s: &Session| {
        super::super::edits::located(&base, &super::super::edits::Snapshot::of_build(s, &input))
    };
    let out = driver::compile_for_jit_warm(&mut warm, &opts, &cold_files, located);
    assert!(
        warm.kept.is_some(),
        "the kept check stays for the next reload"
    );
    let mut cold = Session::new();
    cold.counted_seed = Some(velt_vir::CountedSeed::default());
    let cold = driver::compile_for_jit(&mut cold, &opts).unwrap_or_else(|e| panic!("{e:?}"));
    // A warm reload loads and parses nothing again (the kept build's modules, the edited files
    // parsed again).
    let loaded_again = warm.timings.iter().any(|(stage, _)| *stage == "parse");
    let warm = match out.unwrap_or_else(|e| panic!("{e:?}")) {
        // A kept program keeps the functions nothing reaches any more (the ids are the kept
        // lowering's); the cold build drops them.
        Some(mut p) => match warm.splice_refused.take() {
            None => {
                assert!(!loaded_again, "a warm reload loaded the program again");
                velt_opt::remove_unused_functions(&mut p);
                Ok(p.to_string())
            }
            Some(why) => Err(format!("lowered whole: {why}")),
        },
        None => Err(warm.warm_refused.take().unwrap_or_default()),
    };
    let cold = without_static_ids(&cold.to_string());
    let warm = warm.map(|w| without_static_ids(&w));
    if let (Ok(w), Some(dir)) = (&warm, std::env::var_os("VELT_SPLICE_DUMP")) {
        if *w != cold {
            let dir = PathBuf::from(dir);
            std::fs::write(dir.join("warm.vir"), w).expect("written");
            std::fs::write(dir.join("cold.vir"), &cold).expect("written");
        }
    }
    (warm, cold)
}

const WARM_MAIN: &str = "import { label } from \"./util\";
class Tally {
  n: number = 0;
  add(k: number): number { this.n += k; return this.n; }
}
function score(xs: number[]): number {
  let s = 0;
  xs.forEach((x) => { s += x * 2; });
  return s;
}
function main() {
  const t = new Tally();
  t.add(score([1, 2, 3]));
  console.log(label(t.n));
}
";
const WARM_UTIL: &str = "export function label(n: number): string { return `n=${n}`; }\n";

/// A save that edits function bodies alone is checked from the kept check: its program is the
/// cold front end's (dev-warm-front-end.md step 3).
#[test]
fn a_body_edit_reloads_warm_to_the_cold_program() {
    let files = [("main.velt", WARM_MAIN), ("util.velt", WARM_UTIL)];
    for edited in [
        vec![("main.velt", WARM_MAIN.replace("x * 2", "x * 3"))],
        vec![("main.velt", WARM_MAIN.replace("this.n += k", "this.n -= k"))],
        vec![("util.velt", WARM_UTIL.replace("n=", "N="))],
        // A length-changing edit of the last definition of its file moves nothing.
        vec![("util.velt", WARM_UTIL.replace("n=", "count="))],
        // A loop whose counter the debug optimizer narrows to an integer (`numrep`).
        vec![(
            "main.velt",
            WARM_MAIN.replace(
                "let s = 0;",
                "let s = 0;\n  for (let i = 0; i < 10; i++) { s += i; }",
            ),
        )],
        vec![
            ("main.velt", WARM_MAIN.replace("x * 2", "x + 2")),
            ("util.velt", WARM_UTIL.replace("n=", "#=")),
        ],
    ] {
        let edited: Vec<(&str, &str)> = edited.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let (warm, cold) = warm_reload(&files, &edited);
        assert_eq!(warm.expect("a warm reload"), cold, "{edited:?}");
    }
}

/// Anything but a body edit, or a body edit that changes what callers see, needs the cold front
/// end.
#[test]
fn other_edits_reload_cold() {
    let files = [("main.velt", WARM_MAIN), ("util.velt", WARM_UTIL)];
    let refused = |edited: &[(&str, &str)]| match warm_reload(&files, edited).0 {
        Ok(_) => panic!("a warm reload of {edited:?}"),
        Err(why) => why,
    };
    let sig = WARM_MAIN.replace("score(xs: number[])", "score(xs: Array<number>)");
    assert!(refused(&[("main.velt", &sig)]).contains("`score` changed"));
    let added = format!("{WARM_MAIN}function extra() {{}}\n");
    assert!(refused(&[("main.velt", &added)]).contains("outside the functions"));
    let util = "export function label(n: number) { return n; }\n";
    let why = refused(&[("util.velt", util)]);
    assert!(why.contains("label"), "{why}");
}

// `main` first: the edited `area` is the last definition, whose length may change.
const JSON_MAIN: &str = "function main() {
  console.log(show(new Point()), area(new Size()));
}
class Point {
  x: number = 1;
  y: number = 2;
}
class Size {
  w: number = 3;
}
function show(p: Point): string { return JSON.stringify(p); }
function area(s: Size): number { return s.w * 2; }
";

/// With generated code (`JSON.stringify`): a body edit that demands the same instances reloads
/// warm to the cold program; one that demands another needs the generator, and the cold front
/// end.
#[test]
fn a_body_edit_beside_generated_code_reloads_warm() {
    let files = [("main.velt", JSON_MAIN)];
    let same = JSON_MAIN.replace("s.w * 2", "s.w * 3");
    let (warm, cold) = warm_reload(&files, &[("main.velt", &same)]);
    assert_eq!(warm.expect("a warm reload"), cold);
    let other = JSON_MAIN.replace("return s.w * 2;", "return JSON.stringify(s).length;");
    match warm_reload(&files, &[("main.velt", &other)]) {
        (Ok(warm), cold) => assert_eq!(warm, cold),
        (Err(why), _) => assert!(why.contains("generated"), "{why}"),
    }
}

/// `vir` with each `static#N` named by its data, and the statics' own lines left out: a splice
/// adds the data of new literals after the kept lowering's, where a cold lowering numbers them in
/// the order it meets them, and leaves the data of literals edited away unused.
fn without_static_ids(vir: &str) -> String {
    let mut data = std::collections::HashMap::new();
    for line in vir.lines() {
        if let Some(rest) = line.strip_prefix("static#") {
            if let Some((n, def)) = rest.split_once(' ') {
                data.insert(format!("static#{n}"), format!("static[{def}]"));
            }
        }
    }
    let mut out = String::new();
    for line in vir.lines().filter(|l| !l.starts_with("static#")) {
        let mut line = line.to_string();
        let mut at = 0;
        while let Some(i) = line[at..].find("static#") {
            let start = at + i;
            let end = (line[start + 7..].find(|c: char| !c.is_ascii_digit()))
                .map_or(line.len(), |e| start + 7 + e);
            let named = data.get(&line[start..end]).cloned().unwrap_or_default();
            line.replace_range(start..end, &named);
            at = start + named.len();
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// An edit whose calls pass another constant to a function the debug optimizer narrowed by its
/// callers' constants (`numrep` parameter facts) lowers the program whole: the function, not
/// edited, would be optimized otherwise.
#[test]
fn an_edit_changing_a_parameter_fact_is_optimized_whole() {
    const MAIN: &str = "function steps(n: number): number {
  let s = 0;
  for (let i = 0; i < n; i++) { s += i; }
  return s;
}
function other(): number { return 1; }
function main() {
  console.log(steps(5), other());
}
";
    let edited = MAIN.replace("return 1;", "return steps(7000000000);");
    match warm_reload(&[("main.velt", MAIN)], &[("main.velt", &edited)]) {
        (Err(why), _) => assert!(why.contains("parameter"), "{why}"),
        (Ok(_), _) => panic!("the optimizer took a changed parameter fact"),
    }
}

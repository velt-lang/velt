//! Swap or restart, from real sources through the whole pipeline (front end, lowering, JIT):
//! each case compiles a first version, loads it, then reloads a second one without running
//! either.

use std::path::PathBuf;

use velt_codegen_cl::{DevSession, Reload};

use crate::driver::{self, BuildOptions, Session};

fn compile(source: &str) -> velt_vir::vir::Program {
    let opts = BuildOptions {
        input: PathBuf::from("main.vlt"),
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

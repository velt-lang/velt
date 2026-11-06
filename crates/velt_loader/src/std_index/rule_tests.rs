//! The (a1) rule (module docs) over the real std: the modules a reach-time hook would demand
//! today may be served in place; the rest is listed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use velt_common::SourceMap;

use super::*;
use crate::{load_program, LoadOptions};

fn real_std() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std")
}

/// The std modules a program of `src` loads.
fn loaded(src: &str) -> BTreeSet<String> {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("main.velt");
    let mut sm = SourceMap::default();
    let mut diags = vec![];
    let opts = LoadOptions {
        std_root: Some(real_std()),
        root_source: Some(src.to_string()),
        ..Default::default()
    };
    let l = load_program(&mut sm, &root, opts, &mut diags).unwrap();
    assert!(diags.is_empty(), "{src}: {diags:?}");
    l.modules
        .into_iter()
        .filter(|m| m.is_std)
        .map(|m| m.path)
        .collect()
}

/// The programs that load each reach-time hook global (#2161 §3, (a1)) today, by the global.
///
/// `number_text` is not among them: integers print through its hooks everywhere, so it loads with
/// every program and no demand adds it.
const DEMANDS: &[(&str, &str)] = &[
    ("number_float", "const x = 1.5;\nconsole.log(`${x}`);\n"),
    (
        "host_async",
        "declare host(\"m\") async function f(): Promise<void>;\nconsole.log(\"hi\");\n",
    ),
    (
        "record_any",
        "const r: Record<string, string> = {};\nconsole.log(\"hi\");\n",
    ),
];

/// The modules a reach-time demand loads today that break the rule, with the reason: the design
/// (#2161 §2) expected none. `velt/io/error`'s `IoError` extends the prelude's `Error`, a loaded
/// class, so loading it in place would change `Error`'s subclass closure. Until it is moved
/// (into the prelude, or `host_async` demands it through (a2)), it is listed here: a new entry
/// fails this test, and so does one that is gone.
const KNOWN: &[&str] = &["host_async demands std/io/error: class `IoError` extends `Error`"];

#[test]
fn every_module_a_reach_time_hook_demands_may_be_served_in_place() {
    let index = load(&real_std(), None);
    let by_path = |p: &str| {
        index
            .packages
            .iter()
            .flat_map(|pk| &pk.modules)
            .find(|m| m.path == p)
            .unwrap_or_else(|| panic!("{p} is not in the index"))
    };
    let base = loaded("console.log(\"hello\");\n");
    let mut problems = vec![];
    for (global, src) in DEMANDS {
        let added: Vec<String> = loaded(src).difference(&base).cloned().collect();
        // The demand is real: the global and its module load.
        let global_path = format!("std/prelude/global/{global}");
        assert!(added.contains(&global_path), "{global}: {added:?}");
        eprintln!("{global}: {}", added.join(", "));
        for m in &added {
            let module = by_path(m);
            if !module.in_place() {
                let why: Vec<String> = module.violations.iter().map(|v| v.to_string()).collect();
                problems.push(format!("{global} demands {m}: {}", why.join("; ")));
            }
        }
    }
    assert_eq!(problems, KNOWN);
    // The hooks each demand is for are among what it loads.
    for (module, hook) in [
        ("std/lang/hooks/number_text", "int_text"),
        ("std/lang/hooks/number_float", "float_text"),
        ("std/host/promise", "host_await"),
    ] {
        assert!(by_path(module).hooks.iter().any(|h| h == hook), "{module}");
    }
}

/// The modules the rule sends through a new round ((a2)), with the reasons: printed for the
/// record (`cargo test -p velt_loader std_index::rule -- --nocapture`), and checked for the kinds
/// of module the design names.
#[test]
fn modules_that_may_not_be_served_in_place() {
    let index = load(&real_std(), None);
    let modules: Vec<&ModuleIndex> = index.packages.iter().flat_map(|p| &p.modules).collect();
    let failing: Vec<&&ModuleIndex> = modules.iter().filter(|m| !m.in_place()).collect();
    for m in &failing {
        let why: Vec<String> = m.violations.iter().map(|v| v.to_string()).collect();
        eprintln!("{}: {}", m.path, why.join("; "));
    }
    eprintln!(
        "{} of {} modules may not be served in place",
        failing.len(),
        modules.len()
    );
    let path_of = |p: &str| *modules.iter().find(|m| m.path == p).unwrap();
    // The prelude's `extend` modules (the design's "all the `extend` modules").
    for path in [
        "std/prelude/string",
        "std/prelude/array",
        "std/prelude/number",
    ] {
        let m = path_of(path);
        assert!(!m.extends.is_empty(), "{path}");
        assert!(
            m.violations
                .iter()
                .all(|v| matches!(v, Violation::ExtendsForeignType { .. })),
            "{path}: {:?}",
            m.violations
        );
    }
    // A subclass of a prelude class.
    assert_eq!(
        path_of("std/prelude/assert").violations,
        [Violation::SubclassesForeignClass {
            class: "AssertionError".into(),
            base: "Error".into()
        }]
    );
    // An `export type i8x16 = i8x16;` line does not make a built-in vector type the module's own.
    for path in [
        "std/simd/bytes",
        "std/simd/floats",
        "std/simd/masks",
        "std/simd/words",
    ] {
        let m = path_of(path);
        assert!(!m.extends.is_empty(), "{path}");
        assert!(
            m.violations
                .iter()
                .any(|v| matches!(v, Violation::ExtendsForeignType { .. })),
            "{path}: {:?}",
            m.violations
        );
    }
    // A global's import line, and the hook modules, are served in place.
    for path in ["std/prelude/global/to_fixed", "std/lang/hooks/any_record"] {
        assert!(path_of(path).in_place(), "{path}");
    }
}

fn local_subclass(class: &str, base: &str) -> Vec<Violation> {
    vec![Violation::SubclassesForeignClass {
        class: class.into(),
        base: base.into(),
    }]
}

/// Each reason of the rule, on hand-written modules.
#[test]
fn each_reason_of_the_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [
        (
            "ok",
            "import { Base } from \"velt/x\";\n\
             export struct P { x: i64 }\nexport class C implements Base { n: i64 = 0; }\n\
             class D extends C {}\nextend P { len(): i64 { return this.x; } }\n\
             interface I { f(): i64; }\nenum E { A }\ntype T = i64;\n\
             export const K = 1;\nexport function f(): i64 { return K; }\n",
            vec![],
        ),
        (
            "script",
            "export function f(): void {}\nf();\n",
            vec![Violation::TopLevelStatements],
        ),
        (
            "extend",
            "extend<T> T { a(): void {} }\nextend<T> Array<T> { b(): void {} }\n\
             extend Array<u8> implements I { c(): void {} }\n",
            vec![
                Violation::ExtendsForeignType { ty: "*".into() },
                Violation::ExtendsForeignType { ty: "Array".into() },
            ],
        ),
        (
            "subclass",
            "import { Base } from \"velt/x\";\nexport class C extends Base {}\n",
            vec![Violation::SubclassesForeignClass {
                class: "C".into(),
                base: "Base".into(),
            }],
        ),
        (
            "merge",
            "interface I { a(): void; }\ninterface I { b(): void; }\n",
            vec![Violation::MergesInterface { name: "I".into() }],
        ),
        (
            "extend_alias",
            "export type V = V;\nextend V { a(): void {} }\n",
            vec![Violation::ExtendsForeignType { ty: "V".into() }],
        ),
        (
            "subclass_alias",
            "type B = Error;\nclass C extends B {}\n",
            vec![Violation::SubclassesForeignClass {
                class: "C".into(),
                base: "B".into(),
            }],
        ),
        (
            "local_subclass",
            "export function f(): void { class L extends Error {} }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_binding",
            "export function f(): void { const L = class extends Error {}; }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_method",
            "export class K { m(): void { if (true) { class L extends Error {} } } }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_arrow",
            "export const g = (): void => { class L extends Error {} };\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_arrow_default",
            "export const g = (x: i64 = (() => { class L extends Error {} return 1; })()): i64 => x;\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_object_method",
            "export const o = { *[Symbol.iterator]() { class L extends Error {} } };\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_switch",
            "export function f(n: i64): void { switch (n) { case 1: class L extends Error {} } }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_alias_shadows_own_class",
            "class Base {}\nexport function f(): void { type Base = Error; class L extends Base {} }\n",
            local_subclass("L", "Base"),
        ),
        (
            "local_class_of_another_function",
            "function f(): void { class Error {} }\n\
             export function g(): void { class L extends Error {} }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_static_block",
            "export class K { static { class L extends Error {} } }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_field",
            "export class K { f: () => void = () => { class L extends Error {} }; }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_interface_default",
            "export interface I { m(): void { class L extends Error {} } }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_destructuring_default",
            "export function f(): void {\n\
             const [a = () => { class L extends Error {} }] = [];\n\
             }\n",
            local_subclass("L", "Error"),
        ),
        (
            "local_in_parameter_pattern",
            "export function f({ a = () => { class L extends Error {} } }: T): void {}\n",
            local_subclass("L", "Error"),
        ),
        (
            "extend_interface_of_a_namespace",
            "namespace N { export interface I { a(): void; } }\nextend N.I { z(): void {} }\n",
            vec![],
        ),
        (
            "extend_alias_of_a_namespace",
            "namespace N { export type A = string; }\nextend N.A { z(): void {} }\n",
            vec![Violation::ExtendsForeignType { ty: "N.A".into() }],
        ),
        (
            "local_own",
            "class Own {}\n\
             export function f(): void {\n\
             class A extends Own {}\n\
             function g(): void { class B extends A {} }\n\
             try { class D {} class E extends D {} } catch { class D {} class E extends D {} }\n\
             interface I { a(): void; }\n\
             }\n\
             export function h(): void { interface I { b(): void; } }\n",
            vec![],
        ),
        (
            "merge_in_namespace",
            "namespace N { export interface I { a(): void; } }\n\
             namespace N { export interface I { b(): void; } }\n",
            vec![Violation::MergesInterface { name: "N.I".into() }],
        ),
        (
            "merge_in_nested_namespace",
            "namespace A.B { export interface I { a(): void; } }\n\
             namespace A { export namespace B { export interface I { b(): void; } } }\n",
            vec![Violation::MergesInterface {
                name: "A.B.I".into(),
            }],
        ),
        (
            "same_name_in_two_namespaces",
            "interface I { a(): void; }\n\
             namespace M { export interface I { b(): void; } }\n\
             namespace N { export interface I { c(): void; } }\n",
            vec![],
        ),
        (
            "merge_in_body",
            "export function f(): void {\n\
             interface I { a(): void; }\n\
             interface I { b(): void; }\n\
             }\n",
            vec![Violation::MergesInterface { name: "I".into() }],
        ),
        (
            "global",
            "declare global { interface Array<T> { x(): void; } }\n",
            vec![Violation::AugmentsGlobal],
        ),
        (
            "augment",
            "declare module \"velt/x\" { interface Base { y(): void; } }\n",
            vec![Violation::AugmentsModule {
                spec: "velt/x".into(),
            }],
        ),
    ];
    for (name, src, _) in &files {
        std::fs::write(tmp.path().join(format!("{name}.velt")), src).unwrap();
    }
    let index = load(tmp.path(), None);
    for (name, src, want) in &files {
        let path = format!("std/{name}");
        // A case the parser rejects would test its recovery, not the rule.
        let (_, diags) = velt_syntax::parse_file(crate::std_ast::NO_FILE, src);
        assert!(diags.is_empty(), "{path}: {diags:?}");
        let m = index.packages[0]
            .modules
            .iter()
            .find(|m| m.path == path)
            .unwrap();
        assert_eq!(&m.violations, want, "{path}");
    }
}

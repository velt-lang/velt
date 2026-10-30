//! Standard-library code checked on demand (#1658, docs/internals/design/lazy-std.md): the free
//! functions and constants of a std module no user module imports are checked when checked code
//! reaches them; the rest stay unchecked, with stubs listed in `Program::unchecked`.

mod common;

use common::hir_walk::def_id;
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, Program};

/// The names of the unchecked definitions of `p`.
fn unchecked(p: &Program) -> Vec<String> {
    let mut out: Vec<String> = (p.unchecked.iter())
        .map(|d| match p.def(*d) {
            Def::Fn(f) => {
                assert!(f.body.block.stmts.is_empty() && f.body.block.value.is_none());
                f.name.clone()
            }
            Def::Global(g) => g.name.clone(),
            _ => panic!("an unchecked definition is a function or a constant"),
        })
        .collect();
    out.sort();
    out
}

#[test]
fn what_no_code_reaches_stays_unchecked() {
    let p = ok_src("function main() {\n  console.log(\"hi\");\n}\n");
    let names = unchecked(&p);
    // The prelude imports `velt/time/format`, whose `formatPattern` only `velt/time`
    // calls, and hello world doesn't import that.
    for f in [
        "std/time/format::formatPattern",
        "std/time/civil::monthLong",
    ] {
        assert!(names.contains(&f.to_string()), "{f}: {names:?}");
    }
    // Modules lowering looks into by name are checked whole, and so are the prelude's helpers
    // and its types' methods.
    let eager = ["std/lang/sort", "std/lang/proxy", "std/lang/typed_array"];
    for n in &names {
        assert!(n.starts_with("std/"), "{n}");
        assert!(!eager.iter().any(|e| n.starts_with(e)), "{n}");
    }
    assert!(
        !names.iter().any(|n| n.ends_with("::__anyLooseEq")),
        "a helper"
    );
    // `Number` of the prelude is a function nothing in hello world calls.
    assert!(
        names.contains(&"std/prelude/string::Number".to_string()),
        "{names:?}"
    );
}

#[test]
fn what_checked_code_calls_is_checked_with_what_it_calls() {
    // `velt/time`, imported, is checked whole; its methods call `formatPattern`, which calls
    // the module's private `token`, which calls `monthLong` of another lazy module.
    let p = ok_src(
        "import * as dt from \"velt/time\";\n\
         function main() {\n  console.log(\"hi\");\n}\n",
    );
    let names = unchecked(&p);
    for f in [
        "std/time/format::formatPattern",
        "std/time/format::token",
        "std/time/civil::monthLong",
    ] {
        assert!(!names.contains(&f.to_string()), "{f} is checked: {names:?}");
        assert!(matches!(p.def(def_id(&p, f)), Def::Fn(f) if !f.body.block.stmts.is_empty()));
    }
}

#[test]
fn a_module_the_program_imports_is_checked_whole() {
    let p = ok_src(
        "import { formatPattern } from \"velt/time/format\";\n\
         function main() {\n  console.log(\"hi\");\n}\n",
    );
    let names = unchecked(&p);
    assert!(
        !names.iter().any(|n| n.starts_with("std/time/format::")),
        "{names:?}"
    );
}

#[test]
fn errors_in_user_code_are_reported_as_before() {
    // The program's own unreached function is checked: lazy checking is for std only.
    let e = err_src(
        "import { parseIso } from \"velt/time/parse\";\n\
         function unused(): i64 { return parseIso(1); }\n\
         function main() {}\n",
    );
    assert!(e.contains("error"), "{e}");
}

/// A std module `velt/zzprobe` that re-exports what `velt/zzprobe/impl` (`impl_src`) declares,
/// so that `impl` stays lazy when the program imports `velt/zzprobe`; `src` imports it.
fn with_probe_std(impl_src: &str, src: &str) -> Program {
    let mut l = common::programs::load_src(src);
    let reexport = "export { Gadget, SpecialError, Base, Special } from \"velt/zzprobe/impl\";\n";
    for (path, text) in [("std/zzprobe", reexport), ("std/zzprobe/impl", impl_src)] {
        let file = l.sm.add(format!("{path}.vlt"), text.to_string());
        let (ast, diags) = velt_syntax::parse_file(file, text);
        assert!(diags.is_empty(), "parse errors in {path}");
        l.modules.push(velt_sema::SourceModule {
            path: path.to_string(),
            is_std: true,
            file,
            ast,
            imports: vec![],
            jsx_runtime: None,
        });
    }
    let n = l.modules.len();
    l.modules[n - 2]
        .imports
        .push(("velt/zzprobe/impl".into(), "std/zzprobe/impl".into()));
    l.modules[l.root]
        .imports
        .push(("velt/zzprobe".into(), "std/zzprobe".into()));
    let (p, d) = l.check();
    assert!(
        p.is_some() && d.iter().all(|d| !d.is_error()),
        "unexpected diagnostics:\n{}",
        l.render(&d)
    );
    p.unwrap()
}

/// The classes of the probe's lazy module.
const PROBE_IMPL: &str = "\
export class Gadget { n: number = 1; constructor() {} toString(): string { return \"G\" + this.n; } }
export class SpecialError extends Error {
  constructor() { super(\"special\"); }
  toJSON(): { s: string } { return { s: \"x\" }; }
  __inspect(): string { return \"S\"; }
}
export class Base { constructor() {} }
export class Special extends Base { constructor() { super(); } describe(): string { return \"special\"; } }
";

#[test]
fn a_class_used_only_as_a_type_argument_has_its_methods_checked() {
    // `probe<Gadget>` prints a `Gadget`, so lowering builds `Gadget.toString`; no expression or
    // local of the program has a type that mentions `Gadget`, only the call's type argument.
    let p = with_probe_std(
        PROBE_IMPL,
        "import { Gadget } from \"velt/zzprobe\";\n\
         function probe<T>(): number {\n\
           const xs: T[] = [];\n\
           for (const x of xs) { console.log(`${x}`); console.log(x); }\n\
           return xs.length;\n\
         }\n\
         function main() { console.log(probe<Gadget>()); }\n",
    );
    let names = unchecked(&p);
    assert!(
        !names.contains(&"std/zzprobe/impl::Gadget.toString".to_string()),
        "{names:?}"
    );
}

#[test]
fn the_subclasses_of_a_used_class_have_their_methods_checked() {
    // Printing an `Error` dispatches over every class that extends it, constructed or not.
    let p = with_probe_std(
        PROBE_IMPL,
        "import { Gadget } from \"velt/zzprobe\";\n\
         function main() { const e: Error = new Error(\"x\"); console.log(e); }\n",
    );
    let names = unchecked(&p);
    for m in ["__inspect", "toJSON"] {
        let f = format!("std/zzprobe/impl::SpecialError.{m}");
        assert!(!names.contains(&f), "{f}: {names:?}");
    }
    // Hello world doesn't use `Base`, so its subclass stays unchecked.
    assert!(
        names.contains(&"std/zzprobe/impl::Special.describe".to_string()),
        "{names:?}"
    );
}

#[test]
fn a_user_subclass_makes_the_std_subclasses_of_its_base_checked() {
    // `JSON.stringify` of a `Base` value dispatches over `Special` too.
    let p = with_probe_std(
        PROBE_IMPL,
        "import { Base } from \"velt/zzprobe\";\n\
         class Mine extends Base { constructor() { super(); } }\n\
         function main() { const b: Base = new Mine(); console.log(JSON.stringify(b)); }\n",
    );
    let names = unchecked(&p);
    assert!(
        !names.contains(&"std/zzprobe/impl::Special.describe".to_string()),
        "{names:?}"
    );
}

#[test]
fn the_defaults_of_an_unused_class_use_no_type() {
    // `Holder` is never used, so neither the type of its field default (`Engine | null`) nor
    // that of its constructor's parameter default makes `Engine` used: the prelude's
    // `JsonValue` holds its tape this way, and a program that parses no JSON checks none of
    // the tape's code.
    let impl_src = format!(
        "{PROBE_IMPL}\
export class Engine {{ constructor() {{}} run(): number {{ return 1; }} }}
export class Holder {{
  e: Engine | null = null;
  constructor(e: Engine | null = null) {{ this.e = e; }}
  get(): number {{ return 0; }}
}}
"
    );
    let p = with_probe_std(
        &impl_src,
        "import { Gadget } from \"velt/zzprobe\";\n\
         function main() { console.log(`${new Gadget()}`); }\n",
    );
    let names = unchecked(&p);
    for f in [
        "std/zzprobe/impl::Engine.run",
        "std/zzprobe/impl::Holder.get",
    ] {
        assert!(names.contains(&f.to_string()), "{f}: {names:?}");
    }
    assert!(
        !names.contains(&"std/zzprobe/impl::Gadget.toString".to_string()),
        "{names:?}"
    );
}

#[test]
fn a_decoder_checks_the_tape_only_for_a_type_with_a_json_value() {
    // The reader names no tape: `JsonValue.__fromReader` parses one, and is checked once the
    // decoded type holds a `JsonValue` (it is a method of a used `JsonValue`).
    let tape = "std/encoding/json/tape::parseWith";
    let plain = ok_src(
        "function main() {\n  const p = JSON.parse<{ x: i64 }>(\"{}\");\n  console.log(p.x);\n}\n",
    );
    assert!(unchecked(&plain).contains(&tape.to_string()));
    let value = ok_src(
        "function main() {\n  const p = JSON.parse<{ v: JsonValue }[]>(\"[]\");\n  console.log(p.length);\n}\n",
    );
    assert!(!unchecked(&value).contains(&tape.to_string()));
}

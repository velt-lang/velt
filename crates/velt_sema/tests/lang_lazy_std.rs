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
    // Modules lowering looks into by name, and the prelude's helpers, are checked only for a
    // program with the values lowering calls them for: hello world sorts no array and holds no
    // `any`.
    for f in [
        "std/lang/sort/stable::stableSort",
        "std/lang/hooks/any_ops::anyLooseEq",
    ] {
        assert!(names.contains(&f.to_string()), "{f}: {names:?}");
    }
    for n in &names {
        assert!(n.starts_with("std/"), "{n}");
    }
    // `Number` of the prelude is a function nothing in hello world calls.
    assert!(
        names.contains(&"std/prelude/string::Number".to_string()),
        "{names:?}"
    );
}

/// A field default is the program's code whichever class holds it: it converts `1`, a `number`.
#[test]
fn the_text_of_a_number_in_a_field_default_checks_the_float_printer() {
    let p = ok_src(
        "class K {\n  label: string = \"k\" + String(1);\n}\n\
         function main() {\n  console.log(new K().label);\n}\n",
    );
    let names = unchecked(&p);
    let float = "std/lang/hooks/number_float::floatPush".to_string();
    assert!(!names.contains(&float), "{names:?}");
}

#[test]
fn a_root_lowering_calls_for_a_value_is_checked_with_the_value() {
    let p = ok_src(
        "function main() {\n  const xs = [\"b\", \"a\"];\n  xs.sort();\n  \
         const a: any = xs[0];\n  console.log(a == \"a\");\n}\n",
    );
    let names = unchecked(&p);
    for f in [
        "std/lang/sort/stable::stableSort",
        "std/lang/hooks/any_ops::anyLooseEq",
    ] {
        assert!(!names.contains(&f.to_string()), "{f}: {names:?}");
    }
}

#[test]
fn what_checked_code_calls_is_checked_with_what_it_calls() {
    // `format` calls `formatPattern`, which calls the module's private `token`, which calls
    // `monthLong` of another module.
    let p = ok_src(
        "import { DateTime } from \"velt/time\";\n\
         function main() {\n  console.log(DateTime.fromEpochMs(0).format(\"MMMM\"));\n}\n",
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
    let (l, (p, d)) = probe_check(impl_src, src, false);
    assert!(
        p.is_some() && d.iter().all(|d| !d.is_error()),
        "unexpected diagnostics:\n{}",
        l.render(&d)
    );
    p.unwrap()
}

/// [`with_probe_std`]'s program, std checked lazily or (`eager`) whole, as `velt check --std`.
fn probe_check(
    impl_src: &str,
    src: &str,
    eager: bool,
) -> (
    common::programs::Loaded,
    (Option<Program>, velt_common::Diagnostics),
) {
    let mut l = common::programs::load_src(src);
    let reexport = match impl_src.contains("probeTwice") {
        true => "export { Gadget, probeTwice } from \"velt/zzprobe/impl\";\n",
        false => "export { Gadget, SpecialError, Base, Special } from \"velt/zzprobe/impl\";\n",
    };
    for (path, text) in [("std/zzprobe", reexport), ("std/zzprobe/impl", impl_src)] {
        let file = l.sm.add(format!("{path}.velt"), text.to_string());
        let (ast, diags) = velt_syntax::parse_file(file, text);
        assert!(diags.is_empty(), "parse errors in {path}");
        l.modules.push(velt_sema::SourceModule {
            path: path.to_string(),
            is_std: true,
            is_dependency: false,
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
    // The impl's key of the inspect hook (loaded with the prelude's hooks).
    l.modules[n - 1]
        .imports
        .push(("velt/lang/hooks/keys".into(), "std/lang/hooks/keys".into()));
    l.modules[l.root]
        .imports
        .push(("velt/zzprobe".into(), "std/zzprobe".into()));
    let opts = velt_sema::CheckOptions {
        eager_std: eager,
        ..Default::default()
    };
    let out = velt_sema::check_with(&l.modules, l.root, opts);
    (l, out)
}

/// The classes of the probe's lazy module.
const PROBE_IMPL: &str = "\
import { INSPECT } from \"velt/lang/hooks/keys\";
export class Gadget { n: number = 1; constructor() {} toString(): string { return \"G\" + this.n; } }
export class SpecialError extends Error {
  constructor() { super(\"special\"); }
  toJSON(): { s: string } { return { s: \"x\" }; }
  [INSPECT](): string { return \"S\"; }
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
    for m in [velt_common::std_key!(inspect), "toJSON"] {
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
    // The reader names no tape: `JsonValue[JSON_FROM_READER]` parses one, and is checked once the
    // decoded type holds a `JsonValue` (it is a method of a used `JsonValue`).
    let tape = "std/encoding/json/tape_parse::parseWith";
    let plain = ok_src(
        "function main() {\n  const p = JSON.parse<{ x: i64 }>(\"{}\");\n  console.log(p.x);\n}\n",
    );
    assert!(unchecked(&plain).contains(&tape.to_string()));
    let value = ok_src(
        "function main() {\n  const p = JSON.parse<{ v: JsonValue }[]>(\"[]\");\n  console.log(p.length);\n}\n",
    );
    assert!(!unchecked(&value).contains(&tape.to_string()));
}

#[test]
fn the_tuple_elements_methods_are_checked_only_for_a_tuple_method_call() {
    // Both programs mention `.map`, which loads the module (std/prelude/global/tuple_elements.velt).
    let map = "std/lang/hooks/tuple_elements::TupleElements.map";
    let array = "function main() {\n  console.log([1].map((x) => x + 1));\n}\n";
    assert!(unchecked(&ok_src(array)).contains(&map.to_string()));
    let tuple = "function main() {\n  const t: [number, number] = [1, 2];\n  \
         console.log(t.map((x) => x + 1));\n}\n";
    assert!(!unchecked(&ok_src(tuple)).contains(&map.to_string()));
}

/// An `extend` block on a number type and one on `string` in a lazy std module, with an error
/// in one method, and a free function calling one of them.
const PROBE_EXTEND: &str = "\
export class Gadget { constructor() {} }
export function probeTwice(x: u16): u16 { return x.probeDouble(); }
extend u16 {
  probeDouble(): u16 { return this * (2 as u16); }
  probeBroken(): u16 { return \"not a number\"; }
}
extend string {
  probeShout(): string { return this + \"!\"; }
}
";

#[test]
fn the_preludes_extend_methods_on_primitives_are_checked_when_reached() {
    // Hello world calls none of the prelude's bit methods.
    let names = unchecked(&ok_src("function main() {\n  console.log(\"hi\");\n}\n"));
    for f in [
        "std/prelude/bits::u32.rotateLeft",
        "std/prelude/bits::i64.swapBytes",
    ] {
        assert!(names.contains(&f.to_string()), "{f}: {names:?}");
    }
    // A call and a method value reach them.
    let p = ok_src(
        "function main() {\n\
           const x: u32 = 5;\n\
           console.log(x.rotateLeft(3));\n\
           const y: i64 = 7;\n\
           const f = y.swapBytes;\n\
           console.log(f());\n\
         }\n",
    );
    let names = unchecked(&p);
    for f in [
        "std/prelude/bits::u32.rotateLeft",
        "std/prelude/bits::i64.swapBytes",
    ] {
        assert!(!names.contains(&f.to_string()), "{f} is checked: {names:?}");
        assert!(has_body(&p, f), "{f}");
    }
    assert!(names.contains(&"std/prelude/bits::u32.rotateRight".to_string()));
}

/// Does function `f` of `p` have a checked body (not a stub's empty one)?
fn has_body(p: &Program, f: &str) -> bool {
    matches!(p.def(def_id(p, f)), Def::Fn(f) if f.body.block.value.is_some() || !f.body.block.stmts.is_empty())
}

#[test]
fn an_extend_method_held_by_an_interface_implementation_is_checked() {
    // `compareTo` of a number is called through the bound `Comparable<T>`, never named.
    let p = ok_src(
        "function least<T extends Comparable<T>>(a: T, b: T): T { return a < b ? a : b; }\n\
         function main() {\n  const a: u16 = 3;\n  const b: u16 = 2;\n  console.log(least(a, b));\n}\n",
    );
    let f = "std/prelude/compare::u16.compareTo";
    assert!(!unchecked(&p).contains(&f.to_string()), "{f}");
    assert!(has_body(&p, f), "{f}");
}

#[test]
fn an_error_in_an_unreached_extend_method_is_reported_only_when_std_is_checked_whole() {
    let hello =
        "import { Gadget } from \"velt/zzprobe\";\nfunction main() { console.log(\"hi\"); }\n";
    let (l, (p, d)) = probe_check(PROBE_EXTEND, hello, false);
    assert!(!d.iter().any(|d| d.is_error()), "{}", l.render(&d));
    let names = unchecked(&p.expect("a program"));
    for f in ["u16.probeDouble", "u16.probeBroken", "string.probeShout"] {
        let f = format!("std/zzprobe/impl::{f}");
        assert!(names.contains(&f), "{f}: {names:?}");
    }
    let (l, (_, d)) = probe_check(PROBE_EXTEND, hello, true);
    let all = l.render(&d);
    assert!(
        d.iter().any(|d| d.is_error()) && all.contains("impl.velt"),
        "{all}"
    );
}

#[test]
fn an_extend_method_std_code_calls_is_checked_with_its_caller() {
    // `probeTwice` (a deferred free function) calls `probeDouble`: reaching it reaches both.
    let p = with_probe_std(
        PROBE_EXTEND,
        "import { probeTwice } from \"velt/zzprobe\";\n\
         function main() { const x: u16 = 4; console.log(probeTwice(x)); console.log(\"a\".probeShout()); }\n",
    );
    let names = unchecked(&p);
    for f in ["u16.probeDouble", "string.probeShout"] {
        let f = format!("std/zzprobe/impl::{f}");
        assert!(!names.contains(&f), "{f} is checked: {names:?}");
        assert!(has_body(&p, &f), "{f}");
    }
    assert!(names.contains(&"std/zzprobe/impl::u16.probeBroken".to_string()));
    // Calling the broken one reports its error.
    let (l, (_, d)) = probe_check(
        PROBE_EXTEND,
        "import { Gadget } from \"velt/zzprobe\";\n\
         function main() { const x: u16 = 4; console.log(x.probeBroken()); }\n",
        false,
    );
    assert!(d.iter().any(|d| d.is_error()), "{}", l.render(&d));
}

/// The record conversion hooks (`velt_sema` `lazy_std::RECORD_HOOKS`), loaded as the global a
/// program mentioning `Record` loads.
const RECORD_HOOKS: [&str; 2] = [
    "std/lang/hooks/any_record::anyToRecordAny",
    "std/lang/hooks/any_record::anyToRecord",
];

#[test]
fn the_record_hooks_are_checked_only_for_a_program_that_may_convert_to_a_record() {
    let load = "";
    // A record, and no `any` in the program's code: nothing converts to it.
    let p = ok_src(&format!(
        "{load}function main() {{\n  const r: Record<string, number> = {{ a: 1 }};\n  console.log(r.a);\n}}\n"
    ));
    let names = unchecked(&p);
    for h in RECORD_HOOKS {
        assert!(names.contains(&h.to_string()), "{h} is checked: {names:?}");
    }
    // A record and an `any` that has nothing to do with it, and a record of numbers put into
    // an `any`: still nothing converts to a record.
    let p = ok_src(&format!(
        "{load}function main() {{\n  const r: Record<string, number> = {{ a: 1 }};\n  const x: any = 5;\n  const y: any = r;\n  console.log(r.a, x, y);\n}}\n"
    ));
    let names = unchecked(&p);
    for h in RECORD_HOOKS {
        assert!(
            names.contains(&h.to_string()),
            "{h} is checked with an unrelated any"
        );
    }
    // `any` converted to a record, explicitly; through a member written through `any` (the
    // glue converts it to the field's type); to an object type holding a record.
    for body in [
        "const x: any = { a: 1 };\n  const r = x as Record<string, any>;\n  console.log(r.a);",
        "const n: { props: Record<string, any> } = { props: {} };\n  const h: any = n;\n  h.props = { id: 1 };\n  console.log(n.props.id);",
        "const x: any = { props: { a: 1 } };\n  const n = x as { props: Record<string, number> };\n  console.log(n.props.a);",
        "const n = conv<Record<string, number>>({ a: 1 });\n  console.log(n.a);\n}\nfunction conv<T>(x: any): T {\n  return x;",
    ] {
        let p = ok_src(&format!("{load}function main() {{\n  {body}\n}}\n"));
        let names = unchecked(&p);
        for h in RECORD_HOOKS {
            assert!(!names.contains(&h.to_string()), "{h} is unchecked: {body}");
            assert!(has_body(&p, h), "{h}: {body}");
        }
    }
}

#[test]
fn number_printing_is_checked_once_the_program_prints_a_number() {
    // The integer hooks lowering calls for a number in a template or `String()` are checked once
    // live code has a construct whose lowering calls them (velt_common `hook_demand.rs`; more in
    // tests/live_hooks.rs). The float ones are on demand (velt_common
    // `std_hooks::ON_DEMAND_FLOAT`): checked once the program's own code converts a value that
    // may hold a float to text.
    let int_hook = "std/lang/hooks/number_text::intPush";
    let int_body = "std/lang/number/integer::pushI64";
    let number_hook = "std/lang/hooks/number_text::numberPush";
    let float_hook = "std/lang/hooks/number_float::floatPush";
    let float_body = "std/lang/number/format::pushF64";
    let all = [int_hook, int_body, number_hook, float_hook, float_body];
    // Is `f` in `p` with its body checked?
    let checked = |p: &Program, f: &str| {
        p.defs
            .iter()
            .any(|d| matches!(d, Def::Fn(x) if x.name == f))
            && !unchecked(p).contains(&f.to_string())
    };
    let plain = ok_src("function main() {\n  console.log(\"hi\");\n}\n");
    for f in all {
        assert!(!checked(&plain, f), "{f} is checked in a plain program");
    }
    let ints = ok_src("function main() {\n  const n: i64 = 7;\n  console.log(`n=${n}`);\n}\n");
    for f in [int_hook, int_body] {
        assert!(checked(&ints, f), "{f} is not checked");
    }
    for f in [number_hook, float_hook, float_body] {
        assert!(!checked(&ints, f), "{f} is checked for an integer");
    }
    // A float through a `number` parameter, which nothing in the source marks as one: its type
    // says that it may hold one, and the float printer is checked.
    let unmarked = ok_src(
        "function show(x: number): string {\n  return `x=${x}`;\n}\nfunction main() {\n  console.log(show(7));\n}\n",
    );
    for f in all {
        assert!(checked(&unmarked, f), "{f} is not checked");
    }
    let floats = ok_src("function main() {\n  const x = 0.5;\n  console.log(`x=${x}`);\n}\n");
    for f in all {
        assert!(checked(&floats, f), "{f} is not checked");
    }
}

#[test]
fn a_value_that_cannot_hold_a_float_does_not_check_the_float_printer() {
    let float_hook = "std/lang/hooks/number_float::floatPush";
    let checked = |p: &Program| {
        p.defs
            .iter()
            .any(|d| matches!(d, Def::Fn(x) if x.name == float_hook))
            && !unchecked(p).contains(&float_hook.to_string())
    };
    // Declarations, and the type of the `v` converted.
    let cases = |decls: &str, ty: &str| {
        let src = format!(
            "{decls}\nfunction text(v: {ty}): string {{\n  return `v=${{v}}`;\n}}\nfunction main() {{}}\n"
        );
        ok_src(&src)
    };
    let enums = "enum Dir { Up = \"UP\", Down = \"DOWN\" }\nenum Plain { A, B }\ntype Nest = string | Nest[];\nclass Shape { w: number = 1.5; }";
    let mut wrong = vec![];
    for ty in [
        "string",
        "string | null",
        "string | undefined",
        "string | boolean",
        "string | i64",
        "string[]",
        "(string | null)[]",
        "string[][]",
        "[string, i64]",
        "[string, boolean] | null",
        "\"a\" | \"b\"",
        "Dir",
        "Dir | null",
        "Dir[]",
        "Plain",
        "Nest",
    ] {
        if checked(&cases(enums, ty)) {
            wrong.push(format!("checked for `{ty}`"));
        }
    }
    for ty in [
        "number",
        "f32",
        "number | null",
        "string | number",
        "number[]",
        "(string | number)[]",
        "[string, number]",
        "[string, number] | null",
        "Shape",
        "Shape | null",
        "Shape[]",
    ] {
        if !checked(&cases(enums, ty)) {
            wrong.push(format!("not checked for `{ty}`"));
        }
    }
    assert!(wrong.is_empty(), "the float printer is {wrong:?}");
    // A type parameter may be instantiated with a float.
    let p = ok_src(
        "function text<T>(v: T): string {\n  return `v=${v}`;\n}\nfunction main() {\n  console.log(text(\"a\"));\n}\n",
    );
    assert!(checked(&p), "the float printer is not checked for `T`");
}

/// A std body first checked inside a try that is undone (an arrow tried against each member of
/// a union of function types) numbers its closures as a check up front does: `classObject`'s
/// one closure is `{closure#0}`, with its two captures, and so is the arrow's (#1838). Lowering
/// finds that closure by its name to print a class object.
#[test]
fn closures_made_in_an_undone_try_keep_their_numbers() {
    let p = ok_src(
        "class A {}\n\
         type F = ((x: number) => any) | ((x: number) => string);\n\
         const f: F = (x: number) => {\n  const c: any = A;\n  return c;\n};\n\
         console.log(f(1));\n",
    );
    let closures = |parent: &str| -> Vec<(String, usize, bool)> {
        let prefix = format!("{parent}::{{closure#");
        (p.defs.iter())
            .filter_map(|d| match d {
                Def::Fn(f)
                    if f.name.starts_with(&prefix) && !f.name[prefix.len()..].contains("::") =>
                {
                    Some((
                        f.name.clone(),
                        f.captures.len(),
                        f.body.block.stmts.is_empty() && f.body.block.value.is_none(),
                    ))
                }
                _ => None,
            })
            .collect()
    };
    let hook = "std/lang/hooks/class_object::classObject";
    assert_eq!(
        closures(hook),
        vec![(format!("{hook}::{{closure#0}}"), 2, false)]
    );
    let main = closures("main");
    assert_eq!(main.len(), 1, "{main:?}");
    assert_eq!(main[0].0, "main::{closure#0}");
}

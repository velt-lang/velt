//! The live set is a superset of the functions lowering builds (velt_sema
//! `body/lazy_std/live.rs`): each row is a program where lowering builds something no code names,
//! one row per rule of the live set, and lowering asserts it was live (`Cx::assert_live`) in a
//! compiler with assertions on, as this test's is. Each row fails if its rule is taken out.

use velt_sema::hir;

use super::{lower_ok, std_src};

/// Lower `src`, asserting that the check was on: an empty result would prove nothing.
fn lowers(src: &str) {
    let p = std_src::check_src(src);
    assert!(p.live.is_some(), "the live set was not worked out:\n{src}");
    lower_ok(&p);
}

/// Is a function named `name` in the program's live set?
fn is_live(p: &hir::Program, name: &str) -> bool {
    let live = p.live.as_ref().expect("the live set");
    (p.defs.iter().enumerate()).any(|(i, d)| {
        matches!(d, hir::Def::Fn(f) if f.name == name) && live.contains(&hir::DefId(i as u32))
    })
}

#[test]
fn an_any_converted_to_an_array_is_an_array_view() {
    let src = "function main() {\n  const a: any = [1, 2];\n  const xs = a as number[];\n  \
               xs.push(3);\n  console.log(xs.length);\n}\n";
    lowers(src);
    let p = std_src::check_src(src);
    assert!(is_live(&p, "std/lang/hooks/array_view::arrayViewNew"));
    assert!(is_live(&p, "std/prelude/array_edit::T[].truncate"));
}

#[test]
fn the_text_of_an_any_may_be_an_error() {
    lowers("function main() {\n  const a: any = 1;\n  console.log(String(a));\n}\n");
}

/// `gate` left `uncaughtHeader` closed for this one: no `Error` in the program's types.
#[test]
fn a_thrown_unknown_may_be_an_error() {
    let src = "function main() {\n  const e: unknown = \"x\";\n  throw e;\n}\n";
    lowers(src);
    let p = std_src::check_src(src);
    assert!(is_live(&p, "std/lang/hooks/error::uncaughtHeader"));
    assert!(is_live(&p, "std/prelude/errors::Error.toString"));
}

#[test]
fn a_generator_behind_an_interface_is_a_generator() {
    lowers(
        "function* nums(): Iterator<i64> {\n  yield 1;\n}\n\
         function main() {\n  const g = nums();\n  console.log(g.next());\n}\n",
    );
}

#[test]
fn an_async_generator_behind_an_interface_is_an_async_generator() {
    lowers(
        "async function* nums(): AsyncIterator<i64> {\n  yield 1;\n}\n\
         async function main() {\n  const g = nums();\n  console.log(await g.next());\n}\n",
    );
}

/// Lowering calls `hostAwait`, gated on `HostSettled`, which no code names. Only the live set is
/// looked at: lowering a host function needs a host target (`veltc`'s `live_check` lowers one).
#[test]
fn an_async_host_function_is_awaited_through_a_host_settled() {
    let p = std_src::check_src(
        "declare host(\"env\") async function f(): Promise<i32>;\n\
         function main() {\n  spawn(f());\n}\n",
    );
    assert!(is_live(&p, "std/host/promise::hostAwait"));
}

/// `make_dynamic_impls` runs after the last `reach`: its functions are in the program's module,
/// and this row needs them walked.
#[test]
fn a_function_converted_through_any_is_live() {
    lowers(
        "interface Src {\n  get: () => string;\n}\n\
         function main() {\n  const s: any = Object.create(null);\n  \
         s.get = (): any => \"a\";\n  console.log((s as Src).get());\n}\n",
    );
}

#[test]
fn an_object_converted_to_an_interface_through_any_is_live() {
    lowers(
        "interface I {\n  n: number;\n  show(): string;\n}\n\
         function main() {\n  const o = { n: 1, show: () => \"s\" };\n  const a: any = o;\n  \
         console.log((a as I).show());\n}\n",
    );
}

/// The adapter of `Tree<number>` to `Tree<any>`, and what it calls (`T[].map`).
#[test]
fn an_adapter_to_any_arguments_is_live() {
    lowers(
        "interface Tree<T> {\n  value: T;\n  kids: Tree<T>[] | null;\n  label(): string;\n}\n\
         class N<T> implements Tree<T> {\n  value: T;\n  kids: Tree<T>[] | null = null;\n  \
         constructor(v: T) {\n    this.value = v;\n  }\n  label(): string {\n    return \"n\";\n  \
         }\n}\n\
         function main() {\n  const t = new N<number>(1);\n  const b: Tree<any> = t;\n  \
         console.log(b.value, b.label());\n}\n",
    );
}

/// `hooks::check_shapes` makes the adapter of a `toString` with another shape.
#[test]
fn the_adapter_of_a_to_string_is_live() {
    lowers(
        "class R {\n  toString(radix: i64 = 10): string {\n    return \"r\";\n  }\n}\n\
         function main() {\n  const a: any = new R();\n  console.log(`${a}`);\n}\n",
    );
}

/// `Ctx::upcast_tables` makes the setters after every check.
#[test]
fn the_forwarders_of_an_upcast_are_live() {
    lowers(
        "interface A {\n  x: number;\n  y: number;\n  f(): number;\n}\n\
         interface B {\n  x: number;\n  f(): number;\n}\n\
         class K implements A {\n  x = 1;\n  y = 2;\n  f() {\n    return this.x;\n  }\n}\n\
         function main() {\n  const a: A = new K();\n  const b: B = a;\n  b.x = 5;\n  \
         console.log(a.x, b.x, b.f());\n}\n",
    );
}

/// `queued` is read by no code a program reaches, and lowering runs its initializer.
#[test]
fn a_module_slot_is_live() {
    lowers(
        "import { expect } from \"velt/test\";\n\
         function main() {\n  expect(1).toEqual(1);\n}\n",
    );
}

/// The check catches what it is for: without one definition it stops, naming it.
#[test]
#[should_panic(expected = "built by lowering but not live")]
fn what_lowering_builds_that_is_not_live_is_an_ice() {
    let mut p = std_src::check_src(
        "function f(): i32 {\n  return 1;\n}\nfunction main() {\n  console.log(f());\n}\n",
    );
    let i = (p.defs.iter())
        .position(|d| matches!(d, hir::Def::Fn(f) if f.name == "f"))
        .expect("f");
    let live = p.live.as_mut().expect("the live set");
    assert!(live.remove(&hir::DefId(i as u32)));
    lower_ok(&p);
}

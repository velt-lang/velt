//! JSX lowering (docs/contracts/jsx.md) through the golden test runtimes
//! (`tests/golden/lang/_jsx_test_provider`: generic; `_jsx_test_precompile`: precompile), and
//! generic arrow functions.

mod common;

use common::hir_walk::{calls, exprs, func};
use common::programs::{load_src_at, repo_root, Loaded};
use velt_sema::hir::{self, Callee, ExprKind as E, Lit, Program};

const GENERIC: &str = "// @jsxImportSource ./_jsx_test_provider\n";
const PRECOMPILE: &str = "// @jsxImportSource ./_jsx_test_precompile\n";

fn load(src: &str) -> Loaded {
    load_src_at(&repo_root().join("tests/golden/lang/main.vlt"), src)
}

fn ok(src: &str) -> Program {
    let l = load(src);
    let (p, d) = l.check();
    assert!(
        p.is_some() && d.iter().all(|d| !d.is_error()),
        "unexpected diagnostics:\n{}",
        l.render(&d)
    );
    p.unwrap()
}

fn err(src: &str) -> String {
    let l = load(src);
    let (p, d) = l.check();
    let r = l.render(&d);
    assert!(p.is_none(), "expected errors, program was accepted:\n{r}");
    r
}

/// Names of the functions `f` calls directly, in pre-order.
fn callees(p: &Program, f: &str) -> Vec<String> {
    calls(func(p, f))
        .into_iter()
        .filter_map(|(c, _)| match c {
            Callee::Def(d, _) => match p.def(*d) {
                hir::Def::Fn(f) => Some(f.name.clone()),
                hir::Def::ExternFn(f) => Some(f.name.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The argument lists of `f`'s calls of runtime function `name`.
fn runtime_calls<'p>(p: &'p Program, f: &str, name: &str) -> Vec<&'p [hir::Expr]> {
    calls(func(p, f))
        .into_iter()
        .filter(|(c, _)| match c {
            Callee::Def(d, _) => matches!(p.def(*d), hir::Def::Fn(g) if g.name.ends_with(&format!("jsx-runtime::{name}"))),
            _ => false,
        })
        .map(|(_, args)| args)
        .collect()
}

fn strings(e: &hir::Expr) -> Vec<String> {
    match &e.kind {
        E::ArrayLit(xs) => xs
            .iter()
            .filter_map(|x| match &x.kind {
                E::Lit(Lit::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

#[test]
fn intrinsic_elements_call_jsx_with_names_values_children_and_key() {
    let p = ok(&format!(
        "{GENERIC}function main() {{ const x = \"a\"; const e = <div class=\"c\" hidden title={{x}}>Hi {{x}}</div>; }}"
    ));
    let jsx = runtime_calls(&p, "main", "jsx");
    assert_eq!(jsx.len(), 1);
    let args = jsx[0];
    assert!(matches!(&args[0].kind, E::Lit(Lit::Str(t)) if t == "div"));
    assert_eq!(strings(&args[1]), ["class", "hidden", "title"]);
    let E::ArrayLit(children) = &args[3].kind else {
        panic!("children array")
    };
    assert_eq!(children.len(), 2);
    assert!(matches!(args[4].kind, E::Lit(Lit::Null)));
}

#[test]
fn keys_are_passed_separately() {
    let p = ok(&format!(
        "{GENERIC}function main() {{ const e = <p key=\"k\" class=\"c\">x</p>; const f = <p key={{3}}>y</p>; }}"
    ));
    let jsx = runtime_calls(&p, "main", "jsx");
    assert_eq!(strings(&jsx[0][1]), ["class"]);
    assert!(
        matches!(&jsx[0][4].kind, E::WrapSome(k) if matches!(&k.kind, E::Lit(Lit::Str(s)) if s == "k"))
    );
    assert!(matches!(&jsx[1][4].kind, E::WrapSome(k) if matches!(k.kind, E::Call { .. })));
}

#[test]
fn precompile_folds_text_and_attributes_into_template_strings() {
    let p = ok(&format!(
        "{PRECOMPILE}function main() {{ const x = \"a\"; const n: i64 = 2; const e = <div class=\"a&b\"><br /><span title={{x}}>Hi <b-x>&lt;{{x}}{{n}}</b-x></span></div>; }}"
    ));
    let t = runtime_calls(&p, "main", "jsxTemplate");
    assert_eq!(t.len(), 1, "one template for the whole tree");
    let (E::ArrayLit(strs), E::ArrayLit(slots)) = (&t[0][0].kind, &t[0][1].kind) else {
        panic!("array arguments")
    };
    assert_eq!((strs.len(), slots.len()), (1, 0), "no Element slots");
    assert!(matches!(strs[0].kind, E::Call { .. }), "a template literal");
    assert_eq!(runtime_calls(&p, "main", "jsxAttr").len(), 1);
    assert_eq!(runtime_calls(&p, "main", "jsxEscape").len(), 2);
    assert!(runtime_calls(&p, "main", "jsx").is_empty());
}

#[test]
fn precompile_static_subtree_is_one_string() {
    let p = ok(&format!(
        "{PRECOMPILE}function main() {{ const e = <p class=\"x\">a &amp; b<br /></p>; }}"
    ));
    let t = runtime_calls(&p, "main", "jsxTemplate");
    assert_eq!(strings(&t[0][0]), ["<p class=\"x\">a &amp; b<br></p>"]);
}

#[test]
fn precompile_slots_are_elements() {
    let p = ok(&format!(
        "{PRECOMPILE}function Card(): JSX.Element {{ return <br />; }}
        function main() {{ const el = <hr-x />; const xs: JSX.Element[] = []; const e = <div><Card />{{el}}{{xs}}</div>; }}"
    ));
    let t = runtime_calls(&p, "main", "jsxTemplate");
    let outer = t
        .iter()
        .find(|a| strings(&a[0]).first().is_some_and(|s| s == "<div>"))
        .expect("outer template");
    assert_eq!(strings(&outer[0]), ["<div>", "", "", "</div>"]);
    let E::ArrayLit(slots) = &outer[1].kind else {
        panic!("slots")
    };
    assert_eq!(slots.len(), 3);
    assert_eq!(
        runtime_calls(&p, "main", "Fragment").len(),
        1,
        "the array child"
    );
}

#[test]
fn precompile_leaves_keyed_and_spread_elements_to_jsx() {
    let p = ok(&format!(
        "{PRECOMPILE}function main() {{ const a = {{ class: \"c\" }}; const e = <ul key=\"k\"><li>one</li><li {{...a}}>two</li></ul>; }}"
    ));
    assert_eq!(runtime_calls(&p, "main", "jsx").len(), 2);
    let t = runtime_calls(&p, "main", "jsxTemplate");
    assert_eq!(t.len(), 1);
    assert_eq!(strings(&t[0][0]), ["<li>one</li>"]);
}

#[test]
fn components_are_passed_uncalled_with_their_props() {
    let p = ok(&format!(
        "{GENERIC}function Card(props: {{ title: string; children: JSX.Element }}): JSX.Element {{ return <div>{{props.title}}{{props.children}}</div>; }}
        function main() {{ const e = <Card title=\"t\"><p>body</p></Card>; }}"
    ));
    assert!(
        !callees(&p, "main").iter().any(|n| n == "Card"),
        "the runtime calls the component, not the compiler"
    );
    let args = runtime_calls(&p, "main", "jsxComponent")[0];
    assert_eq!(args.len(), 4);
    assert!(matches!(args[0].kind, E::Closure(_)), "an adapter closure");
    assert!(matches!(args[1].kind, E::AdtLit { .. }));
    assert!(matches!(&args[3].kind, E::Lit(Lit::Str(s)) if s == "main#Card"));
}

#[test]
fn async_components_are_passed_uncalled() {
    let p = ok(&format!(
        "{GENERIC}async function Later(): Promise<JSX.Element> {{ return <p>later</p>; }}
        function main() {{ const e = <Later />; }}"
    ));
    let args = runtime_calls(&p, "main", "jsxAsyncComponent")[0];
    assert!(matches!(args[0].kind, E::Closure(_)));
    assert!(!callees(&p, "main").iter().any(|n| n == "Later"));
}

#[test]
fn generic_components_infer_their_type_arguments_from_props() {
    let p = ok(&format!(
        "{GENERIC}function List<T>(props: {{ items: T[]; show: (x: T) => string }}): JSX.Element {{ return <ul></ul>; }}
        function main() {{ const e = <List items={{[1, 2]}} show={{(x) => `${{x + 1}}`}} />; }}"
    ));
    assert_eq!(runtime_calls(&p, "main", "jsxComponent").len(), 1);
    let r = err(&format!(
        "{GENERIC}function List<T>(props: {{ n: i64 }}): JSX.Element {{ return <ul></ul>; }}
        function main() {{ const e = <List n={{1}} />; }}"
    ));
    assert_eq!(
        r.matches("cannot infer type parameter `T`").count(),
        1,
        "{r}"
    );
}

#[test]
fn props_that_cannot_be_copied_are_an_error() {
    let r = err(&format!(
        "{GENERIC}function W(props: {{ p: Promise<i64> }}): JSX.Element {{ const p = props.p; return <p></p>; }}
        async function v(): Promise<i64> {{ return 1; }}
        function main() {{ const e = <W p={{v()}} />; }}"
    ));
    assert!(
        r.contains("the props of <W> cannot be copied into the component"),
        "{r}"
    );
    assert!(!r.contains("ICE"), "{r}");
}

#[test]
fn async_components_are_not_floating_promises() {
    ok(&format!(
        "{GENERIC}async function Posts(): Promise<JSX.Element> {{ return <ul></ul>; }}
        async function main() {{ <Posts />; const e = <div><Posts /></div>; }}"
    ));
}

#[test]
fn generated_calls_keep_the_element_span() {
    let src = format!("{GENERIC}function main() {{ const e = <p>x</p>; }}");
    let p = ok(&src);
    let call = exprs(func(&p, "main"))
        .into_iter()
        .find(|e| {
            matches!(
                &e.kind,
                E::Call {
                    callee: Callee::Def(..),
                    ..
                }
            )
        })
        .expect("call");
    let lo = src.find("<p>").unwrap() as u32;
    assert_eq!(
        (call.span.lo, call.span.hi),
        (lo, lo + "<p>x</p>".len() as u32)
    );
}

#[test]
fn ts_worded_attribute_errors() {
    let r = err(&format!(
        "{GENERIC}function main() {{ const a = <div clas=\"x\" />; const b = <blink />; const c = <button onClick={{1}} />; }}"
    ));
    assert!(
        r.contains("Property 'clas' does not exist on type 'JSX.IntrinsicElements[\"div\"]'. Did you mean 'class'?"),
        "{r}"
    );
    assert!(
        r.contains("Property 'blink' does not exist on type 'JSX.IntrinsicElements'."),
        "{r}"
    );
    assert!(r.contains("declares no event handlers"), "{r}");
    assert!(!r.contains("ICE"), "{r}");
}

#[test]
fn custom_elements_and_dashed_attributes_are_not_checked() {
    ok(&format!(
        "{GENERIC}function main() {{ const a = <my-widget anything=\"x\" n={{1}} />; const b = <div data-id={{2}} aria-label=\"l\" />; }}"
    ));
}

#[test]
fn jsx_without_a_runtime_is_one_error() {
    let r = err("function main() { const n = 1; const e = <p>{n + \"x\"}</p>; }");
    assert_eq!(r.matches("no JSX runtime was loaded").count(), 1, "{r}");
    assert!(
        r.contains("mismatched types"),
        "expressions are still checked:\n{r}"
    );
}

#[test]
fn module_level_generic_arrows_are_generic_functions() {
    let p =
        ok("const id = <T,>(x: T): T => x;\nfunction main() { console.log(id(1), id(\"a\")); }");
    assert_eq!(func(&p, "id").generics, 1);
}

#[test]
fn other_generic_arrows_are_reported() {
    let r = err("function main() { const id = <T,>(x: T): T => x; console.log(1); }");
    assert!(
        r.contains("a generic arrow function must be a module-level constant"),
        "{r}"
    );
    assert!(!r.contains("unknown type"), "{r}");
}

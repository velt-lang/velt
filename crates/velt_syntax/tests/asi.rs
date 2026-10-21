//! Automatic semicolon insertion: ECMAScript's rules as TypeScript and Node apply them, the
//! restricted productions, and the warning for a line that continues the statement before it.

mod common;

use common::*;
use velt_common::Severity;

/// The statements of `function f() { <src> }`, with the diagnostics.
fn stmts(src: &str) -> (Vec<Stmt>, Vec<(Severity, String)>) {
    let (m, d) = parse(&format!("function f() {{\n{src}\n}}"));
    let diags = d.into_iter().map(|d| (d.severity, d.message)).collect();
    let s = match m.items.first().map(|i| &i.kind) {
        Some(ItemKind::Function(f)) => f.body.stmts.clone(),
        _ => vec![],
    };
    (s, diags)
}

fn expr_stmts(src: &str) -> Vec<String> {
    let (s, d) = stmts(src);
    assert!(d.iter().all(|(sev, _)| *sev != Severity::Error), "{d:?}");
    s.iter()
        .map(|s| match &s.kind {
            StmtKind::Expr(e) => sx(e),
            StmtKind::Return(Some(e)) => format!("return {}", sx(e)),
            StmtKind::Return(None) => "return".into(),
            StmtKind::Break(l) => format!("break {}", l.as_ref().map_or("", |l| &l.name)),
            StmtKind::Var(v) => format!("let {}", pat(&v.pattern)),
            k => format!("{k:?}").chars().take(12).collect(),
        })
        .collect()
}

fn warnings(src: &str) -> usize {
    let (_, d) = stmts(src);
    assert!(d.iter().all(|(sev, _)| *sev != Severity::Error), "{d:?}");
    d.iter().filter(|(s, _)| *s == Severity::Warning).count()
}

#[test]
fn line_break_brace_and_end_of_file_end_a_statement() {
    assert_eq!(
        expr_stmts("let a = 1\nlet b = 2\nb = a"),
        ["let a", "let b", "(= b a)"]
    );
    assert!(parse("let x = 1").1.is_empty());
    assert!(parse("function f() { g() }").1.is_empty());
    assert!(
        parse("import { a } from \"./a\"\nexport { a }\nconst b = a")
            .1
            .is_empty()
    );
    // On one line a `;` is still needed.
    let e = errors("function f() { let a = 1 let b = 2 }");
    assert!(e[0].contains("expected `;`"), "{e:?}");
}

#[test]
fn no_insertion_in_a_for_header() {
    let e = errors("function f() { for (let i = 0\n i < 3\n i++) {} }");
    assert!(e[0].contains("expected `;`"), "{e:?}");
}

#[test]
fn do_while_takes_a_semicolon_on_the_same_line() {
    let (s, d) = stmts("do {} while (false) g()");
    assert!(d.is_empty(), "{d:?}");
    assert_eq!(s.len(), 2);
}

#[test]
fn return_ends_at_a_line_break() {
    assert_eq!(expr_stmts("return\n1"), ["return", "1"]);
    assert_eq!(expr_stmts("return 1 +\n2"), ["return (+ 1 2)"]);
    assert_eq!(
        expr_stmts("if (a) return\n}\nfunction g() {"),
        ["If { cond: E"]
    );
}

#[test]
fn break_and_continue_labels_are_on_the_same_line() {
    assert_eq!(expr_stmts("l: while (a) { break\nl }"), ["Labeled { la"]);
    let (s, _) = stmts("l: while (a) { break\nl }");
    let StmtKind::Labeled { body, .. } = &s[0].kind else {
        panic!()
    };
    let StmtKind::While { body, .. } = &body.kind else {
        panic!()
    };
    assert!(matches!(body.stmts[0].kind, StmtKind::Break(None)));
    assert!(matches!(body.stmts[1].kind, StmtKind::Expr(_)));
}

#[test]
fn throw_needs_its_value_on_the_same_line() {
    let e = errors("function f() { throw\nnew Error(\"x\") }");
    assert!(
        e[0].contains("line break is not permitted after `throw`"),
        "{e:?}"
    );
}

#[test]
fn postfix_update_after_a_line_break_is_the_next_prefix() {
    assert_eq!(expr_stmts("a\n++b"), ["a", "(++pre b)"]);
    assert_eq!(expr_stmts("a++\nb"), ["(++post a)", "b"]);
}

#[test]
fn yield_ends_at_a_line_break() {
    let (m, d) = parse("function* g() {\n  yield\n  1\n}");
    assert!(d.is_empty(), "{d:?}");
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(f.body.stmts.len(), 2);
    let StmtKind::Expr(e) = &f.body.stmts[0].kind else {
        panic!()
    };
    assert!(matches!(e.kind, ExprKind::Yield { arg: None, .. }));
}

#[test]
fn no_line_break_before_an_arrow_or_after_async() {
    let e = errors("const f = (a: number)\n=> a");
    assert!(e.iter().any(|e| e.contains("before `=>`")), "{e:?}");
    let e = errors("const f = a\n=> a");
    assert!(e.iter().any(|e| e.contains("before `=>`")), "{e:?}");
    let e = errors("const f = async\n(a: number) => a");
    assert!(e.iter().any(|e| e.contains("after `async`")), "{e:?}");
}

#[test]
fn continuation_lines_mean_what_they_mean_in_node_and_warn() {
    assert_eq!(expr_stmts("a = b\n(c)"), ["(= a (call b [c]))"]);
    assert_eq!(expr_stmts("a = b\n[c]"), ["(= a ([] b c))"]);
    assert_eq!(expr_stmts("a = b\n+c"), ["(= a (+ b c))"]);
    assert_eq!(expr_stmts("a = b\n-c"), ["(= a (- b c))"]);
    assert_eq!(expr_stmts("a = b\n/c/d"), ["(= a (/ (/ b c) d))"]);
    for src in [
        "a = b\n(c)",
        "a = b\n[c]",
        "a = b\n+c",
        "a = b\n-c",
        "a = b\n/c",
    ] {
        assert_eq!(warnings(src), 1, "{src}");
    }
    // Only one per line: `/c/d` continues once.
    assert_eq!(warnings("a = b\n/c/d"), 1);
    // A line break inside a block comment does not start a line; one in a line comment does.
    assert_eq!(warnings("a = b /* x\n y */ + c"), 0);
    assert_eq!(warnings("a = b /* x */\n+ c"), 1);
    assert_eq!(warnings("a = b // x\n+ c"), 1);
}

#[test]
fn no_warning_inside_brackets_or_on_one_line() {
    for src in [
        "g(a\n(b))",
        "g(a,\n[b][0])",
        "const o = { a: b\n+ c }",
        "const s = `${a\n+ b}`",
        "a = b + c",
        "a = b\n.c(d)",
        "a = cond\n? b\n: c",
        "h(() => {\n  a = b\n  c()\n})",
    ] {
        assert_eq!(warnings(src), 0, "{src}");
    }
}

#[test]
fn a_template_after_an_expression_is_an_error() {
    let e = errors("function f() { a = b\n`c` }");
    assert!(e[0].contains("as a tagged template"), "{e:?}");
    let e = errors("function f() { a = b`c` }");
    assert!(e[0].contains("tagged templates"), "{e:?}");
}

#[test]
fn semicolon_free_code_parses() {
    let src = "import { reactive } from './reactive'\n\
               export enum Flags {\n  SKIP = '__v_skip',\n}\n\
               export function track(target: object, key: unknown) {\n\
               \x20 if (!target) return\n\
               \x20 const dep = new Map()\n\
               \x20 const run = () => {\n    dep.clear()\n  }\n\
               \x20 for (let i = 0; i < 3; i++) run()\n\
               \x20 return dep\n}\n\
               export class Ref<T> {\n  value: T\n  constructor(v: T) {\n    this.value = v\n  }\n}\n";
    let (_, d) = parse(src);
    assert!(
        d.is_empty(),
        "{:?}",
        d.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn recovery_stops_at_a_line_break() {
    // One error each: recovery resumes at the next line instead of the next `;` or `{`.
    let e = errors("class C {\n  x = 1 2\n  constructor(a: Map<string, number>) {}\n}");
    assert_eq!(e.len(), 1, "{e:?}");
    let e = errors("function f() {\n  let x = 1 2\n  g(a, b)\n  h()\n}");
    assert_eq!(e.len(), 1, "{e:?}");
}

#[test]
fn a_conditional_type_does_not_continue_across_a_line_break() {
    let (_, d) = parse("interface O {\n  mixins?: M[]\n  extends?: E\n}");
    assert!(
        d.is_empty(),
        "{:?}",
        d.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn type_arguments_do_not_continue_across_a_line_break() {
    let (m, d) = parse("interface I {\n  a: string\n  (x: number): string\n  <T>(x: T): T\n}");
    assert!(
        d.is_empty(),
        "{:?}",
        d.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let Some(ItemKind::Interface(i)) = m.items.first().map(|i| &i.kind) else {
        panic!("{:?}", m.items)
    };
    assert_eq!(i.fields.len(), 1);
    assert_eq!(i.call_sigs.len(), 2);
    // `Array` / `<number>` is not `Array<number>`, as in TypeScript (TS1109).
    assert!(!parse("type B = Array\n<number>").1.is_empty());
}

#[test]
fn modifiers_before_a_line_break() {
    // In a class, `async` before a line break is a field named `async`; `static`, `get` and
    // `set` still apply to the next line.
    let m = parse_ok("class A {\n  async\n  = 2\n  static\n  s() {}\n  get\n  v() { return 1 }\n  readonly\n  = 3\n}");
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    let fields: Vec<_> = c
        .fields
        .iter()
        .map(|f| (f.name.name.as_str(), f.readonly))
        .collect();
    assert_eq!(fields, [("async", false), ("readonly", false)]);
    let methods: Vec<_> = c
        .methods
        .iter()
        .map(|m| {
            (
                m.decl.sig.name.name.as_str(),
                m.is_static,
                m.decl.sig.is_async,
            )
        })
        .collect();
    assert!(methods.contains(&("s", true, false)), "{methods:?}");
    // In an interface, `readonly` before a line break is a member named `readonly`.
    let e = errors("interface I {\n  readonly\n  x: number\n}");
    assert!(!e.is_empty());
    // In an object literal, `async` before a line break is not a modifier: a syntax error.
    let e = errors("const o = {\n  async\n  foo() { return 1 }\n}");
    assert!(!e.is_empty());
    assert!(parse("const o = { async foo() { return 1 } }").1.is_empty());
}

#[test]
fn declaration_words_before_a_line_break_are_expressions() {
    // `type`, `interface`, `declare` and `namespace` declare only with the name on their line.
    for src in [
        "type\nT = number",
        "declare\nconst x: number",
        "namespace\nN {}",
    ] {
        let m = parse(src).0;
        assert!(
            !m.items.iter().any(|i| matches!(
                i.kind,
                ItemKind::TypeAlias(_) | ItemKind::ExternFn(_) | ItemKind::Namespace(_)
            )),
            "{src}: {:?}",
            m.items
        );
    }
    assert!(!errors("interface\nI {}").is_empty());
    assert!(parse("type T = number\ninterface I { a: number }")
        .1
        .is_empty());
}

#[test]
fn unicode_line_separators_are_line_breaks() {
    // U+2028 and U+2029 are whitespace that ends a line, as in JavaScript.
    assert_eq!(expr_stmts("return\u{2028}1"), ["return", "1"]);
    assert_eq!(expr_stmts("a\u{2029}++b"), ["a", "(++pre b)"]);
    assert!(parse("// c\u{2028}const x = 1").1.is_empty());
}

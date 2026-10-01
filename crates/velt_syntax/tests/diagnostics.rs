//! Diagnostics wording and positions, error recovery, NodeId uniqueness and span accuracy.

mod common;

use common::*;
use std::collections::HashSet;

#[test]
fn diagnostic_wording() {
    let e = errors("function f() { let x = 1 }");
    assert!(e[0].contains("expected `;`"), "{:?}", e);
    let e = errors("function f() { foo( }");
    assert!(e[0].contains("expected expression"), "{:?}", e);
    let e = errors("function f() { let = 3; }");
    assert!(e[0].contains("expected pattern"), "{:?}", e);
    let e = errors("function f() {");
    assert!(
        e[0].contains("expected `}`") && e[0].contains("end of file"),
        "{:?}",
        e
    );
    let e = errors("console.log(1);");
    assert!(e[0].contains("expected item"), "{:?}", e);
    let e = errors("}");
    assert!(e[0].contains("unexpected `}`"), "{:?}", e);
}

#[test]
fn expected_semicolon_points_at_offending_token() {
    let src = "function f() {\n  let x = 1\n  let y = 2;\n}";
    let (_, d) = parse(src);
    assert_eq!(d.len(), 1, "{:?}", d);
    let lo = d[0].labels[0].span.lo as usize;
    assert_eq!(&src[lo..lo + 3], "let");
}

#[test]
fn recovers_and_reports_multiple_errors() {
    let src = "function a() {\n  let x = (1 + ;\n  let y = 2 3;\n  z = ;\n  ok();\n}\nfunction b(: i64 {}\nfunction c() { fine(); }\n";
    let (m, d) = parse(src);
    let msgs: Vec<_> = d.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(d.len(), 4, "{:?}", msgs);
    assert!(msgs[0].contains("expected expression"));
    assert!(msgs[1].contains("expected `;`"));
    assert!(msgs[2].contains("expected expression"));
    // Functions a and c survive; a still contains `ok();`.
    let names: Vec<_> = m
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Function(f) => Some(f.sig.name.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["a", "c"]);
    let ItemKind::Function(a) = &m.items[0].kind else {
        panic!()
    };
    assert!(a
        .body
        .stmts
        .iter()
        .any(|s| matches!(&s.kind, StmtKind::Expr(e) if sx(e) == "(call ok [])")));
}

#[test]
fn recovery_inside_class_members() {
    let (m, d) = parse("class A { x: ; y: i32; foo( {} bar(): void {} }\nfunction after() {}");
    assert!(!d.is_empty());
    let ItemKind::Class(c) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(c.fields.len(), 1);
    assert_eq!(c.fields[0].name.name, "y");
    assert!(c.methods.iter().any(|m| m.decl.sig.name.name == "bar"));
    assert!(matches!(&m.items[1].kind, ItemKind::Function(f) if f.sig.name.name == "after"));
}

#[test]
fn lexer_errors_do_not_stop_parsing() {
    let (m, d) = parse("function f() { let s = \"abc;\n let t = 1; }\nfunction g() {}");
    assert!(d.iter().any(|d| d.message.contains("unterminated string")));
    assert_eq!(m.items.len(), 2);
}

#[test]
fn node_ids_are_unique() {
    let m = parse_ok(KITCHEN_SINK);
    // Collect ids via the debug dump to cover every nested node without a full visitor.
    let dumped = dump(&m);
    let mut ids = HashSet::new();
    let mut count = 0;
    for part in dumped.split("id: NodeId(").skip(1) {
        let n: u32 = part[..part.find(')').unwrap()]
            .trim()
            .trim_end_matches(',')
            .parse()
            .unwrap();
        count += 1;
        assert!(ids.insert(n), "duplicate NodeId({})", n);
    }
    assert!(count > 200, "only {} ids", count);
}

#[test]
fn spans_are_accurate() {
    let src = "function main(): i32 {\n  const total = add(1, 2) * 3;\n  return total;\n}";
    let m = parse_ok(src);
    let text = |sp: velt_common::Span| &src[sp.lo as usize..sp.hi as usize];
    assert_eq!(text(m.items[0].span), src);
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(text(f.sig.span), "function main(): i32");
    assert_eq!(text(f.sig.name.span), "main");
    assert_eq!(text(f.sig.ret.as_ref().unwrap().span), "i32");
    let StmtKind::Var(v) = &f.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(text(f.body.stmts[0].span), "const total = add(1, 2) * 3;");
    assert_eq!(text(v.span), "const total = add(1, 2) * 3");
    assert_eq!(text(v.pattern.span), "total");
    let init = v.init.as_ref().unwrap();
    assert_eq!(text(init.span), "add(1, 2) * 3");
    let ExprKind::Binary { lhs, rhs, .. } = &init.kind else {
        panic!()
    };
    assert_eq!(text(lhs.span), "add(1, 2)");
    assert_eq!(text(rhs.span), "3");
    let ExprKind::Call { callee, args, .. } = &lhs.kind else {
        panic!()
    };
    assert_eq!(text(callee.span), "add");
    assert_eq!(text(args[1].span), "2");
    assert_eq!(text(f.body.span), &src[src.find('{').unwrap()..]);
    assert_eq!(m.span.hi as usize, src.len());

    let src2 = "function f() { const s = `a ${x} b`; const g = (a: i64) => a; }";
    let m = parse_ok(src2);
    let text2 = |sp: velt_common::Span| &src2[sp.lo as usize..sp.hi as usize];
    let s = body(&m);
    let StmtKind::Var(v) = &s[0].kind else {
        panic!()
    };
    assert_eq!(text2(v.init.as_ref().unwrap().span), "`a ${x} b`");
    let StmtKind::Var(v) = &s[1].kind else {
        panic!()
    };
    assert_eq!(text2(v.init.as_ref().unwrap().span), "(a: i64) => a");
}

#[test]
fn file_id_propagates_into_spans() {
    let (m, d) = parse_file(FileId(7), "function f() { x(; }");
    assert_eq!(m.span.file, FileId(7));
    assert_eq!(d[0].labels[0].span.file, FileId(7));
}

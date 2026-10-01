//! JSX: context-sensitive `<`, tags and names, attributes, children text with React's whitespace
//! rules and HTML entities, expression containers (with regex and template literals inside),
//! fragments, spreads, generic arrows `<T,>`, the `@jsxImportSource` pragma, and diagnostics.

mod common;

use common::*;

/// The JSX rendering of the single expression `src` (see `common::jsx`).
fn j(src: &str) -> String {
    sx(&expr(src))
}

/// Parses `const x = <src>;` and returns the rendering of the initializer.
fn init(src: &str) -> String {
    let m = parse_ok(&format!("const x = {src};"));
    match &m.items[0].kind {
        ItemKind::Var(v) => sx(v.init.as_ref().expect("initializer")),
        k => panic!("expected const, got {k:?}"),
    }
}

#[test]
fn elements_and_self_closing() {
    assert_eq!(j("<div></div>"), "<div/>");
    assert_eq!(j("<br />"), "<br/>");
    assert_eq!(j("<br/>"), "<br/>");
    assert_eq!(j("<p>hi</p>"), r#"<p>["hi"]"#);
    assert_eq!(
        j("<ul><li>a</li><li>b</li></ul>"),
        r#"<ul>[<li>["a"] <li>["b"]]"#
    );
}

#[test]
fn fragments() {
    assert_eq!(j("<></>"), "</>");
    assert_eq!(j("<><a /><b /></>"), "<>[<a/> <b/>]");
    assert_eq!(init("<>text</>"), r#"<>["text"]"#);
}

#[test]
fn member_namespaced_and_dashed_names() {
    assert_eq!(j("<ui.Card />"), "<ui.Card/>");
    assert_eq!(j("<a.b.c></a.b.c>"), "<a.b.c/>");
    assert_eq!(j("<svg:rect />"), "<svg:rect/>");
    assert_eq!(j("<my-element></my-element>"), "<my-element/>");
    let e = expr("<ui.Card />");
    let ExprKind::Jsx(el) = &e.kind else {
        panic!("not JSX")
    };
    assert!(matches!(&el.name, Some(JsxName::Member(parts)) if parts.len() == 2));
}

#[test]
fn attributes() {
    assert_eq!(
        j(r#"<input type="text" disabled value={v} />"#),
        r#"<input type="text" disabled value={v}/>"#
    );
    assert_eq!(j("<a href='x'></a>"), r#"<a href="x"/>"#);
    assert_eq!(
        j(r#"<div data-id="1" aria-label="l" />"#),
        r#"<div data-id="1" aria-label="l"/>"#
    );
    assert_eq!(
        j(r##"<use xlink:href="#a" />"##),
        r##"<use xlink:href="#a"/>"##
    );
    assert_eq!(
        j(r#"<label for="a" class="b" />"#),
        r#"<label for="a" class="b"/>"#
    );
    assert_eq!(j("<A x=<b /> />"), "<A x=<b/>/>");
    assert_eq!(j("<div {...props} a={1} />"), "<div {...props} a={1}/>");
}

#[test]
fn attribute_strings_have_no_escapes_and_decode_entities() {
    assert_eq!(j(r#"<a title="C:\dir" />"#), r#"<a title="C:\\dir"/>"#);
    assert_eq!(
        j(r#"<a title="a &amp; b &quot;c&quot;" />"#),
        r#"<a title="a & b \"c\""/>"#
    );
    assert_eq!(
        j("<a title=\"two\nlines\" />"),
        r#"<a title="two\nlines"/>"#
    );
}

#[test]
fn children_whitespace_rules() {
    assert_eq!(j("<p>  a  </p>"), r#"<p>["  a  "]"#);
    assert_eq!(j("<p>\n  hello\n  world\n</p>"), r#"<p>["hello world"]"#);
    assert_eq!(j("<p>\n  <b />\n</p>"), "<p>[<b/>]");
    assert_eq!(j("<p>a <b /> c</p>"), r#"<p>["a " <b/> " c"]"#);
    assert_eq!(j("<p>{a} {b}</p>"), r#"<p>[{a} " " {b}]"#);
    assert_eq!(j("<p>{a}\n  {b}</p>"), "<p>[{a} {b}]");
    assert_eq!(j("<p>a   b</p>"), r#"<p>["a   b"]"#);
    assert_eq!(j("<p>\t</p>"), r#"<p>[" "]"#);
}

#[test]
fn children_entities() {
    assert_eq!(
        j("<p>&lt;tag&gt; &amp; &#65;&#x42;</p>"),
        r#"<p>["<tag> & AB"]"#
    );
    assert_eq!(j("<p>&nbsp;</p>"), "<p>[\"\\u{a0}\"]");
    assert_eq!(j("<p>&bogus; &</p>"), r#"<p>["&bogus; &"]"#);
}

#[test]
fn text_is_not_code() {
    // Comment markers, quotes and keywords in text are just text.
    assert_eq!(
        j("<p>see http://x.y/z, don't /* stop */ return</p>"),
        r#"<p>["see http://x.y/z, don't /* stop */ return"]"#
    );
}

#[test]
fn expression_containers() {
    assert_eq!(j("<p>{x + 1}</p>"), "<p>[{(+ x 1)}]");
    assert_eq!(j("<p>{}</p>"), "<p>[{}]");
    assert_eq!(j("<p>{/* note */}</p>"), "<p>[{}]");
    assert_eq!(j("<p>{// note\n}</p>"), "<p>[{}]");
    assert_eq!(j("<p>{...items}</p>"), "<p>[{...items}]");
    assert_eq!(j("<p>{{ a: 1 }}</p>"), "<p>[{{a: 1}}]");
    assert_eq!(
        j("<ul>{xs.map((x) => <li>{x}</li>)}</ul>"),
        "<ul>[{(call (. xs map) [(arrow (x) <li>[{x}])])}]"
    );
}

#[test]
fn regex_and_templates_inside_containers() {
    assert_eq!(
        j("<p>{/a+b/.test(s) ? `x${<b>{y}</b>}z` : \"no\"}</p>"),
        r#"<p>[{(? (call (. (new RegExp ["a+b" ""]) test) [s]) `x${<b>[{y}]}z` "no")}]"#
    );
    assert_eq!(j("<a b={`t${c}`} />"), "<a b={`t${c}`}/>");
}

#[test]
fn jsx_in_expression_positions() {
    assert_eq!(j("c ? <a /> : <b />"), "(? c <a/> <b/>)");
    assert_eq!(j("ok && <a />"), "(&& ok <a/>)");
    assert_eq!(j("f(<a />, [<b />])"), "(call f [<a/> [<b/>]])");
    assert_eq!(j("() => <a />"), "(arrow () <a/>)");
    let m = parse_ok("function f() { return <div>hi</div>; }");
    assert!(
        matches!(&body(&m)[0].kind, StmtKind::Return(Some(e)) if matches!(e.kind, ExprKind::Jsx(_)))
    );
    // Parentheses around an element are layout only.
    assert_eq!(j("(<a />)"), "<a/>");
    assert_eq!(j("(<a />) && b"), "(&& <a/> b)");
}

#[test]
fn less_than_and_generic_calls_are_unchanged() {
    assert_eq!(j("a < b"), "(< a b)");
    assert_eq!(j("a<b"), "(< a b)");
    assert_eq!(j("i < n && n > 0"), "(&& (< i n) (> n 0))");
    assert_eq!(j("x<y>(z)"), "(call x<y> [z])");
    assert_eq!(j("a.f<Map<K, V>>(z)"), "(call (. a f)<Map<K, V>> [z])");
    assert_eq!(j("a <= b"), "(<= a b)");
    assert_eq!(j("a << b"), "(<< a b)");
    assert_eq!(j("f(a) < g(b)"), "(< (call f [a]) (call g [b]))");
}

#[test]
fn after_a_jsx_element_an_operand_has_ended() {
    // `/` after an element is division (not a regex), `<` a comparison (not JSX).
    let (_, d) = parse("const a = <b /> / 2;");
    assert!(d.iter().all(|d| !d.message.contains("regular")), "{d:?}");
}

#[test]
fn generic_arrows() {
    assert_eq!(init("<T,>(x: T) => x"), "(arrow <T>(x: T) x)");
    assert_eq!(
        init("<T, U>(x: T, y: U): U => y"),
        "(arrow <T, U>(x: T, y: U): U y)"
    );
    assert_eq!(
        init("<T extends Show>(x: T) => x"),
        "(arrow <T extends Show>(x: T) x)"
    );
    assert_eq!(init("async <T,>(x: T) => x"), "(async arrow <T>(x: T) x)");
    assert_eq!(j("f(<T,>(x: T) => x)"), "(call f [(arrow <T>(x: T) x)])");
}

#[test]
fn generic_arrow_without_comma_is_jsx() {
    // As in `.tsx`: `<T>(x: T) => x` starts an element `T`.
    let errs = errors("const f = <T>(x: T) => x;");
    assert!(
        errs.iter()
            .any(|e| e == "JSX element 'T' has no corresponding closing tag."),
        "{errs:?}"
    );
}

#[test]
fn import_source_pragma() {
    let m = parse_ok("// @jsxImportSource sigx\nconst a = <b />;");
    assert_eq!(m.jsx_import_source.as_deref(), Some("sigx"));
    let m = parse_ok("/** @jsxImportSource std/jsx */\nconst a = 1;");
    assert_eq!(m.jsx_import_source.as_deref(), Some("std/jsx"));
    let m = parse_ok("// header\n// @jsxImportSource ./local\nconst a = 1;");
    assert_eq!(m.jsx_import_source.as_deref(), Some("./local"));
    // Only comments before the first token count.
    let m = parse_ok("const a = 1;\n// @jsxImportSource late\n");
    assert_eq!(m.jsx_import_source, None);
    let m = parse_ok("// @jsxImportSourcex nope\nconst a = 1;");
    assert_eq!(m.jsx_import_source, None);
}

#[test]
fn spans_cover_the_source() {
    let src = "const x = <p a=\"1\">hi {y}</p>;";
    let m = parse_ok(src);
    let ItemKind::Var(v) = &m.items[0].kind else {
        panic!()
    };
    let ExprKind::Jsx(el) = &v.init.as_ref().unwrap().kind else {
        panic!()
    };
    let at = |s: velt_common::Span| &src[s.lo as usize..s.hi as usize];
    assert_eq!(at(el.span), "<p a=\"1\">hi {y}</p>");
    let JsxAttr::Named {
        value: Some(JsxAttrValue::Str { span, .. }),
        ..
    } = &el.attrs[0]
    else {
        panic!()
    };
    assert_eq!(at(*span), "\"1\"");
    let JsxChild::Text { span, .. } = &el.children[0] else {
        panic!()
    };
    assert_eq!(at(*span), "hi ");
    let JsxChild::Expr { span, .. } = &el.children[1] else {
        panic!()
    };
    assert_eq!(at(*span), "{y}");
}

#[test]
fn closing_tag_errors() {
    let errs = errors("const a = <div><span></div>;");
    assert!(
        errs.iter()
            .any(|e| e == "Expected corresponding JSX closing tag for 'span'."),
        "{errs:?}"
    );
    let errs = errors("const a = <div>text");
    assert!(
        errs.iter()
            .any(|e| e == "JSX element 'div' has no corresponding closing tag."),
        "{errs:?}"
    );
    let errs = errors("const a = <>x</div>;");
    assert!(
        errs.contains(&"Expected corresponding closing tag for JSX fragment.".to_string()),
        "{errs:?}"
    );
    let errs = errors("const a = <><b/>");
    assert!(
        errs.contains(&"JSX fragment has no corresponding closing tag.".to_string()),
        "{errs:?}"
    );
}

#[test]
fn other_errors() {
    let errs = errors("const a = <a b={} />;");
    assert_eq!(
        errs,
        ["JSX attributes must only be assigned a non-empty expression."]
    );
    let errs = errors("const a = <p>a > b</p>;");
    assert_eq!(errs, ["Unexpected token. Did you mean `{'>'}` or `&gt;`?"]);
    let errs = errors("const a = <p>}</p>;");
    assert_eq!(
        errs,
        ["Unexpected token. Did you mean `{'}'}` or `&rbrace;`?"]
    );
    assert!(!errors("const a = <a 1 />;").is_empty());
    assert!(!errors("const a = <a b= />;").is_empty());
    assert!(!errors("const a = <a {b} />;").is_empty());
    assert!(!errors("const a = <a.b:c />;").is_empty());
}

#[test]
fn errors_recover_at_the_next_item() {
    let (m, d) = parse("const a = <div><span></div>;\n");
    assert!(!d.is_empty());
    let _ = m;
    let (m, d) = parse("function f() { return <a b={} />; }\nfunction g() {}\n");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(m.items.len(), 2);
}

#[test]
fn comment_ranges_skip_jsx_text_and_strings() {
    let src = "// a\nconst x = <p t='//'>don't // no {/* b */}</p>; /* c */";
    let found: Vec<&str> = velt_syntax::comment_ranges(src)
        .into_iter()
        .map(|r| &src[r.start as usize..r.end as usize])
        .collect();
    assert_eq!(found, ["// a", "/* b */", "/* c */"]);
}

//! Lexical tests: numeric literals, string escapes, template literals, comments.

mod common;

use common::*;

#[test]
fn numeric_literals() {
    check("123", "123");
    check("1_000_000", "1000000");
    check("0xff", "255");
    check("0XFF_FF", "65535");
    check("0b1010", "10");
    check("0o17", "15");
    check("1.5", "1.5");
    check("1e21", "1e21");
    check("2.5e-3", "0.0025");
    check("1E+2", "100.0");
    check(".5", "0.5");
    check("10u8", "10u8");
    check("5i32", "5i32");
    check("1.0f32", "1.0f32");
    check("3f64", "3.0f64");
    check("0xffu8", "255u8");
    check(
        "340282366920938463463374607431768211455",
        "340282366920938463463374607431768211455",
    );
    // Past the digits lexed in 64 bits (#1658): the full u64 range, a separator and digits
    // beyond, up to u128, in every base.
    check("0xeef453d6923bd65a", "17218479456385750618");
    check("0xffffffffffffffff", "18446744073709551615");
    check("0x1_0000_0000_0000_0000", "18446744073709551616");
    check(
        "0xffffffffffffffffffffffffffffffff",
        "340282366920938463463374607431768211455",
    );
    check("0o1777777777777777777777", "18446744073709551615");
    check(
        "0b1111111111111111111111111111111111111111111111111111111111111111",
        "18446744073709551615",
    );
    check("0x0000000000000000000000ff", "255");
    let e = expr("1.5");
    assert!(matches!(e.kind, ExprKind::Lit(Lit::Float { value, suffix: None }) if value == 1.5));
}

#[test]
fn numeric_literal_errors() {
    assert!(errors("const x = 340282366920938463463374607431768211456;")[0].contains("too large"));
    assert!(errors("const x = 10u7;")[0].contains("invalid suffix"));
    assert!(errors("const x = 1.5i32;")[0].contains("invalid suffix"));
    assert!(errors("const x = 0b102;")[0].contains("invalid digit"));
    assert!(errors("const x = 0x;")[0].contains("missing digits"));
    assert!(
        errors("const x = 0x1_0000_0000_0000_0000_0000_0000_0000_0000;")[0].contains("too large")
    );
    assert!(
        errors("const x = 0b1111111111111111111111111111111111111111111111111111111111112;")[0]
            .contains("invalid digit")
    );
}

#[test]
fn string_escapes() {
    let e = expr(r#""a\nb\r\t\\\"\'\0\x41\u{1F600}é""#);
    assert_eq!(sx(&e), format!("{:?}", "a\nb\r\t\\\"'\0A\u{1F600}é"));
    let e = expr(r#"'single "quoted"'"#);
    assert_eq!(sx(&e), format!("{:?}", "single \"quoted\""));
    let e = expr("\"héllo €\"");
    assert_eq!(sx(&e), format!("{:?}", "héllo €"));
    assert!(errors("const s = \"abc;")[0].contains("unterminated string"));
    assert!(errors(r#"const s = "\xZZ";"#)[0].contains("Hexadecimal digit expected."));
    assert!(errors(r#"const s = "\u{110000}";"#)[0].contains("between 0x0 and 0x10FFFF"));
}

/// Escapes cook as in ECMAScript strict code (#1265): a character that is not an escape stands
/// for itself, `\b \v \f` are control characters, a backslash before a line break continues
/// the line, and a surrogate pair written as two `\u` escapes is one character.
#[test]
fn string_escapes_ecmascript() {
    let e = expr(r#""a\d\q\ \é""#);
    assert_eq!(sx(&e), format!("{:?}", "adq é"));
    let e = expr(r#""\b\v\f\0|\$\`""#);
    assert_eq!(sx(&e), format!("{:?}", "\u{8}\u{b}\u{c}\0|$`"));
    let e = expr("\"a\\\nb\\\r\nc\\\rd\\\u{2028}e\"");
    assert_eq!(sx(&e), format!("{:?}", "abcde"));
    let e = expr(r#""\uD83D\uDE00\u0041""#);
    assert_eq!(sx(&e), format!("{:?}", "\u{1F600}A"));
    for pair in [
        r#""\u{D83D}\u{DE00}""#,
        r#""\uD83D\u{DE00}""#,
        r#""\u{D83D}\uDE00""#,
    ] {
        assert_eq!(sx(&expr(pair)), format!("{:?}", "\u{1F600}"), "{pair}");
    }
    assert!(errors(r#"const s = "\u{D83D}x";"#)[0].contains("lone surrogate"));
    let e = expr(
        r#"`\d${x}\q\
z`"#,
    );
    let ExprKind::Template { quasis, .. } = &e.kind else {
        panic!()
    };
    assert_eq!(quasis, &vec!["d".to_string(), "qz".to_string()]);
}

/// Escapes strict code rejects, with tsc's messages.
#[test]
fn string_escape_errors() {
    let octal = |src: &str| errors(src)[0].clone();
    assert_eq!(
        octal(r#"const s = "\1";"#),
        "Octal escape sequences are not allowed. Use the syntax '\\x01'."
    );
    assert!(octal(r#"const s = "\101";"#).contains("'\\x41'"));
    assert!(octal(r#"const s = "\08";"#).contains("'\\x00'"));
    assert!(octal(r#"const s = `a\377`;"#).contains("'\\xff'"));
    assert_eq!(errors(r#"const s = "\377\400";"#).len(), 2);
    assert_eq!(
        octal(r#"const s = "\8";"#),
        "Escape sequence '\\8' is not allowed."
    );
    assert!(octal(r#"const s = "\u12";"#).contains("Hexadecimal digit expected."));
    assert!(octal(r#"const s = `\u{}`;"#).contains("Hexadecimal digit expected."));
    assert!(octal(r#"const s = "\u{12";"#).contains("Unterminated Unicode escape sequence."));
    assert!(octal(r#"const s = "\uD800";"#).contains("lone surrogate"));
    assert!(errors(r#"const s = "\0";"#).is_empty());
}

#[test]
fn template_literals() {
    check("`plain`", "`plain`");
    check("`a ${x} b ${y + 1} c`", "`a ${x} b ${(+ y 1)} c`");
    check("`${x}`", "`${x}`");
    check("`a ${`b ${c}`} d`", "`a ${`b ${c}`} d`");
    check("`${ {k: 1}.k }`", "`${(. {k: 1} k)}`");
    check(r"`esc \` \$ \${x} \n`", "`esc ` $ ${x} \n`");
    let e = expr("`multi\nline`");
    let ExprKind::Template { quasis, exprs } = &e.kind else {
        panic!()
    };
    assert_eq!(quasis, &vec!["multi\nline".to_string()]);
    assert!(exprs.is_empty());
    let e = expr("`a${1}b${2}c`");
    let ExprKind::Template { quasis, exprs } = &e.kind else {
        panic!()
    };
    assert_eq!(quasis.len(), exprs.len() + 1);
    assert!(errors("const s = `abc;")
        .iter()
        .any(|m| m.contains("unterminated template")));
    // CRLF normalized to LF in template values.
    let e = expr("`a\r\nb`");
    let ExprKind::Template { quasis, .. } = &e.kind else {
        panic!()
    };
    assert_eq!(quasis[0], "a\nb");
}

#[test]
fn comments_and_unexpected_chars() {
    parse_ok("// line\n/* block\n comment */ function f() { /* inline */ return; } // end");
    assert!(errors("/* never closed")
        .iter()
        .any(|m| m.contains("unterminated block comment")));
    let errs = errors("function f() { let x = 1 # 2; }");
    assert!(errs[0].contains("unexpected character `#`"), "{:?}", errs);
    let errs = errors("function f() { let é = 1; }");
    assert!(errs[0].contains("unexpected character"), "{:?}", errs);
}

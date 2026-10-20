//! Operator precedence and associativity (JS rules, `as` like relational, `**` right-assoc).

mod common;

use common::*;

#[test]
fn arithmetic_precedence() {
    check("1 + 2 * 3", "(+ 1 (* 2 3))");
    check("1 * 2 + 3", "(+ (* 1 2) 3)");
    check("1 - 2 - 3", "(- (- 1 2) 3)");
    check(
        "2 + 3 * 4 - (10 - 4) / 2",
        "(- (+ 2 (* 3 4)) (/ (paren (- 10 4)) 2))",
    );
    check("a % b * c", "(* (% a b) c)");
    check("2 ** 3 ** 2", "(** 2 (** 3 2))");
    check("2 * 3 ** 2", "(* 2 (** 3 2))");
    check("-a ** 2", "(- (** a 2))");
    check("-a * b", "(* (- a) b)");
    check("2 ** -1", "(** 2 (- 1))");
    check("-a / b", "(/ (- a) b)");
    check("!a && b", "(&& (! a) b)");
    check("~0", "(~ 0)");
    check("- -a", "(- (- a))");
    check("+a", "(+ a)");
}

#[test]
fn comparison_logic_bitwise_precedence() {
    check("a < b == c > d", "(== (< a b) (> c d))");
    check("a <= b != c >= d", "(!= (<= a b) (>= c d))");
    check("a === b", "(=== a b)");
    check("a !== b", "(!== a b)");
    check("a & b | c ^ d", "(| (& a b) (^ c d))");
    check("a == b & c", "(& (== a b) c)");
    check("a || b && c", "(|| a (&& b c))");
    check("a && b || c", "(|| (&& a b) c)");
    check("a ?? b || c", "(|| (?? a b) c)");
    check("1 << 2 + 3", "(<< 1 (+ 2 3))");
    check("a >> 2 < b", "(< (>> a 2) b)");
    check("a >>> 2", "(>>> a 2)");
    check("-16 >> 2", "(>> (- 16) 2)");
    check("a > b", "(> a b)");
    check("a > -b", "(> a (- b))");
    check(
        "!(a <= b) && true || false",
        "(|| (&& (! (paren (<= a b))) true) false)",
    );
    check("a | b & c", "(| a (& b c))");
}

#[test]
fn as_binds_like_relational() {
    check("x as f64", "(as x f64)");
    check("5 as f64 / 2.0", "(/ (as 5 f64) 2.0)");
    check("a + b as f64", "(as (+ a b) f64)");
    check("a as i64 < b", "(< (as a i64) b)");
    check("x as u8 | y", "(as x (u8 | y))");
    check("(x as u8) | y", "(| (paren (as x u8)) y)");
    check("x as T | null", "(as x (T | null))");
    check("k as \"a\" | \"b\" | null", "(as k (\"a\" | \"b\" | null))");
    check("x as 1 | y", "(as x (1 | y))");
    check("x as A | B<C>[]", "(as x (A | B<C>[]))");
    check("c ? x as T[\"k\"] : y", "(? c (as x T[\"k\"]) y)");
    check(
        "x as
    | A
    | null",
        "(as x (A | null))",
    );
    check("-x as f64", "(as (- x) f64)");
    check(
        "(small as i64) * 100000",
        "(* (paren (as small i64)) 100000)",
    );
    check("x as i32 as i64", "(as (as x i32) i64)");
    check("xs as i64[]", "(as xs i64[])");
    check("x as Array<i64>", "(as x Array<i64>)");
    check(
        "x as Map<K, Array<V>> == y",
        "(== (as x Map<K, Array<V>>) y)",
    );
}

#[test]
fn assignment_and_ternary() {
    check("a = b = c", "(= a (= b c))");
    check("a += 1", "(+= a 1)");
    check("a -= 1", "(-= a 1)");
    check("a *= 1", "(*= a 1)");
    check("a /= 1", "(/= a 1)");
    check("a %= 1", "(%= a 1)");
    check("a **= 1", "(**= a 1)");
    check("a <<= 1", "(<<= a 1)");
    check("a >>= 1", "(>>= a 1)");
    check("a >>>= 1", "(>>>= a 1)");
    check("a &= 1", "(&= a 1)");
    check("a |= 1", "(|= a 1)");
    check("a ^= 1", "(^= a 1)");
    check("a &&= b", "(&&= a b)");
    check("a ||= b", "(||= a b)");
    check("a ??= b", "(??= a b)");
    check("o.x = 1", "(= (. o x) 1)");
    check("a[i] = 1", "(= ([] a i) 1)");
    check("a ? b : c", "(? a b c)");
    check("a ? b : c ? d : e", "(? a b (? c d e))");
    check("a ? b ? c : d : e", "(? a (? b c d) e)");
    check("a || b ? c : d", "(? (|| a b) c d)");
    check("x = a ? b : c", "(= x (? a b c))");
    check(
        "n == 0 ? true : isOdd(n - 1)",
        "(? (== n 0) true (call isOdd [(- n 1)]))",
    );
    check("a ? x = 1 : y", "(? a (= x 1) y)");
    assert!(errors("function f() { 1 = 2; }")[0].contains("invalid assignment target"));
    assert!(errors("function f() { a + b = 2; }")[0].contains("invalid assignment target"));
}

#[test]
fn update_expressions() {
    check("i++", "(++post i)");
    check("i--", "(--post i)");
    check("++i", "(++pre i)");
    check("--i", "(--pre i)");
    check("a.b++", "(++post (. a b))");
    check("-i++", "(- (++post i))");
    check("a + ++b", "(+ a (++pre b))");
}

/// A `?` followed by any token that can start an expression is a ternary, not the removed
/// postfix `?` (`can_start_expr`): every prefix operator and every primary expression.
#[test]
fn ternary_branch_starts() {
    check("c ? void 0 : v", "(? c (void 0) v)");
    check("c ? typeof c : \"n\"", "(? c (typeof c) \"n\")");
    check("c ? !a : b", "(? c (! a) b)");
    check("c ? ~a : b", "(? c (~ a) b)");
    check("c ? -a : b", "(? c (- a) b)");
    check("c ? +a : b", "(? c (+ a) b)");
    check("c ? ++a : b", "(? c (++pre a) b)");
    check("c ? --a : b", "(? c (--pre a) b)");
    check("c ? await p : b", "(? c (await p) b)");
    check("c ? delete o.k : b", "(? c (delete (. o k)) b)");
    check("c ? undefined : b", "(? c undefined b)");
    check("c ? null : b", "(? c null b)");
    check("c ? true : false", "(? c true false)");
    check("c ? this : b", "(? c this b)");
    check("c ? 1 : 2.5", "(? c 1 2.5)");
    check("c ? \"s\" : b", "(? c \"s\" b)");
    check("c ? (a) : b", "(? c (paren a) b)");
    check("c ? [a] : b", "(? c [a] b)");
    check("c ? type : of", "(? c type of)");
    for src in [
        "c ? `t` : b",
        "c ? `${a}` : b",
        "c ? /a+/.test(s) : false",
        "c ? { a: 1 } : b",
        "c ? new A() : b",
        "c ? async () => 1 : b",
        "c ? (x: number) => x : b",
        "c ? <T>(x: T) => x : b",
        "c ? super.m() : b",
    ] {
        let e = expr(src);
        assert!(matches!(e.kind, ExprKind::Cond { .. }), "{src}: {e:?}");
    }
    // A function expression is rejected as such, not as a removed `?`.
    let errs = errors("function f(c: bool) { const g = c ? function () {} : null; }");
    assert!(
        !errs.iter().any(|e| e.contains("operator was removed")),
        "{errs:?}"
    );
    // `#x in o` as a branch.
    let errs =
        errors("class A { #x = 1; has(c: bool, o: A): bool { return c ? #x in o : false; } }");
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn removed_question_operator_vs_optional_chaining() {
    check("f()?.x", "(?. (call f []) x)");
    check("a?.b?.c", "(?. (?. a b) c)");
    check("a?.[0]?.(1)", "(call ([] a 0) [1])");
    check("a ? b : c", "(? a b c)");
    for src in ["function f() { g()?; }", "function f() { h(x?, y); }"] {
        let errs = errors(src);
        assert!(
            errs.iter().any(|e| e == "the `?` operator was removed"),
            "{src}: {errs:?}"
        );
    }
}

#[test]
fn delete_is_a_contextual_prefix_operator() {
    check("delete r[k]", "(delete ([] r k))");
    check("delete this.x", "(delete (. this x))");
    check("!delete r.a", "(! (delete (. r a)))");
    // A parenthesized operand, as in TypeScript.
    check("delete (r[\"a\"])", "(delete ([] r \"a\"))");
    check("delete ((r.a))", "(delete (. r a))");
    check("delete(x)", "(delete (paren x))");
    // Anywhere else `delete` is an ordinary name.
    check("delete + 1", "(+ delete 1)");
    check("m.delete(k)", "(call (. m delete) [k])");
}

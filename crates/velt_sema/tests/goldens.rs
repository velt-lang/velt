//! The M1 golden programs (`tests/golden/m1/*.vlt`) as hand-built ASTs: each must type-check
//! cleanly; error goldens must produce the expected message at the expected position.

mod common;

use common::*;
use velt_sema::hir::{self, Callee, ExprKind as E, Intrinsic, StmtKind as S, TyKind, UseMode};

fn arith() -> Vec<velt_syntax::ast::Item> {
    vec![func(
        "main",
        &[],
        Some("void"),
        vec![
            const_("a", int(7)),
            const_("b", int(3)),
            es(log(vec![
                bin(B::Add, var("a"), var("b")),
                bin(B::Sub, var("a"), var("b")),
                bin(B::Mul, var("a"), var("b")),
                bin(B::Div, var("a"), var("b")),
                bin(B::Rem, var("a"), var("b")),
            ])),
            es(log(vec![
                bin(B::Div, neg(var("a")), var("b")),
                bin(B::Rem, neg(var("a")), var("b")),
            ])),
            es(log(vec![bin(
                B::Sub,
                bin(B::Add, int(2), bin(B::Mul, int(3), int(4))),
                bin(B::Div, paren(bin(B::Sub, int(10), int(4))), int(2)),
            )])),
            const_t("x", "f64", float(1.5)),
            const_("y", float(2.25)),
            es(log(vec![
                bin(B::Add, var("x"), var("y")),
                bin(B::Mul, var("x"), var("y")),
                bin(B::Div, var("y"), var("x")),
            ])),
            es(log(vec![
                float(10.0),
                bin(B::Add, float(0.1), float(0.2)),
                float(1e21),
                bin(B::Div, cast(int(5), "f64"), float(2.0)),
            ])),
            es(log(vec![
                bin(B::Gt, var("a"), var("b")),
                bin(B::Eq, var("a"), var("b")),
                bin(B::NotEq, var("a"), var("b")),
                bin(
                    B::Or,
                    bin(
                        B::And,
                        not(paren(bin(B::LtEq, var("a"), var("b")))),
                        bool_(true),
                    ),
                    bool_(false),
                ),
            ])),
            const_t("small", "i32", int(100000)),
            es(log(vec![
                bin(B::Mul, var("small"), int(3)),
                bin(B::Mul, paren(cast(var("small"), "i64")), int(100000)),
            ])),
            const_t("u", "u8", int(250)),
            es(log(vec![
                bin(B::Add, var("u"), int(5)),
                cast(int(300), "u8"),
            ])),
            es(log(vec![
                bin(B::BitAnd, int(7), int(3)),
                bin(B::BitOr, int(7), int(8)),
                bin(B::BitXor, int(7), int(2)),
                bin(B::Shl, int(1), int(10)),
                bin(B::Shr, neg(int(16)), int(2)),
                bitnot(int(0)),
            ])),
        ],
    )]
}

fn control() -> Vec<velt_syntax::ast::Item> {
    vec![func(
        "main",
        &[],
        None,
        vec![
            let_("i", int(0)),
            let_("sum", int(0)),
            while_(
                bin(B::Lt, var("i"), int(10)),
                vec![
                    es(assign(var("i"), bin(B::Add, var("i"), int(1)))),
                    if_(
                        bin(B::Eq, bin(B::Rem, var("i"), int(2)), int(0)),
                        vec![cont(None)],
                        None,
                    ),
                    es(cassign(B::Add, var("sum"), var("i"))),
                ],
            ),
            es(log(vec![str_("sum odd"), var("sum")])),
            for_(
                Some(let_("j", int(0))),
                Some(bin(B::Lt, var("j"), int(5))),
                Some(post_inc(var("j"))),
                vec![
                    if_(bin(B::Eq, var("j"), int(3)), vec![brk(None)], None),
                    es(log(vec![str_("j"), var("j")])),
                ],
            ),
            let_("n", int(15)),
            if_(
                bin(B::Eq, bin(B::Rem, var("n"), int(15)), int(0)),
                vec![es(log(vec![str_("FizzBuzz")]))],
                Some(if_(
                    bin(B::Eq, bin(B::Rem, var("n"), int(3)), int(0)),
                    vec![es(log(vec![str_("Fizz")]))],
                    Some(block_s(vec![es(log(vec![var("n")]))])),
                )),
            ),
            const_(
                "kind",
                cond(bin(B::Gt, var("n"), int(10)), str_("big"), str_("small")),
            ),
            es(log(vec![var("kind")])),
            let_("k", int(3)),
            do_while(vec![es(post_dec(var("k")))], bin(B::Gt, var("k"), int(0))),
            es(log(vec![var("k")])),
            labeled(
                "outer",
                for_(
                    Some(let_("a", int(0))),
                    Some(bin(B::Lt, var("a"), int(3))),
                    Some(post_inc(var("a"))),
                    vec![for_(
                        Some(let_("b", int(0))),
                        Some(bin(B::Lt, var("b"), int(3))),
                        Some(post_inc(var("b"))),
                        vec![
                            if_(
                                bin(B::Eq, var("b"), int(2)),
                                vec![cont(Some("outer"))],
                                None,
                            ),
                            if_(bin(B::Eq, var("a"), int(2)), vec![brk(Some("outer"))], None),
                            es(log(vec![var("a"), var("b")])),
                        ],
                    )],
                ),
            ),
        ],
    )]
}

fn functions() -> Vec<velt_syntax::ast::Item> {
    vec![
        func(
            "fib",
            &[("n", "i64")],
            Some("i64"),
            vec![
                if_(bin(B::Lt, var("n"), int(2)), vec![ret(var("n"))], None),
                ret(bin(
                    B::Add,
                    call("fib", vec![bin(B::Sub, var("n"), int(1))]),
                    call("fib", vec![bin(B::Sub, var("n"), int(2))]),
                )),
            ],
        ),
        func(
            "gcd",
            &[("a", "i64"), ("b", "i64")],
            Some("i64"),
            vec![
                while_(
                    bin(B::NotEq, var("b"), int(0)),
                    vec![
                        const_("t", var("b")),
                        es(assign(var("b"), bin(B::Rem, var("a"), var("b")))),
                        es(assign(var("a"), var("t"))),
                    ],
                ),
                ret(var("a")),
            ],
        ),
        func(
            "isEven",
            &[("n", "i64")],
            Some("bool"),
            vec![ret(cond(
                bin(B::Eq, var("n"), int(0)),
                bool_(true),
                call("isOdd", vec![bin(B::Sub, var("n"), int(1))]),
            ))],
        ),
        func(
            "isOdd",
            &[("n", "i64")],
            Some("bool"),
            vec![ret(cond(
                bin(B::Eq, var("n"), int(0)),
                bool_(false),
                call("isEven", vec![bin(B::Sub, var("n"), int(1))]),
            ))],
        ),
        func(
            "square",
            &[("x", "f64")],
            Some("f64"),
            vec![ret(bin(B::Mul, var("x"), var("x")))],
        ),
        func(
            "main",
            &[],
            Some("i32"),
            vec![
                es(log(vec![call("fib", vec![int(30)])])),
                es(log(vec![call("gcd", vec![int(1071), int(462)])])),
                es(log(vec![
                    call("isEven", vec![int(10)]),
                    call("isOdd", vec![int(7)]),
                ])),
                es(log(vec![call("square", vec![float(1.5)])])),
                ret(int(3)),
            ],
        ),
    ]
}

fn hello() -> Vec<velt_syntax::ast::Item> {
    vec![func(
        "main",
        &[],
        None,
        vec![es(log(vec![str_("Hello, Velt!")]))],
    )]
}

fn strings() -> Vec<velt_syntax::ast::Item> {
    vec![
        func(
            "greet",
            &[("name", "string")],
            Some("string"),
            vec![ret(tpl(&["Hello, ", "!"], vec![var("name")]))],
        ),
        func(
            "main",
            &[],
            None,
            vec![
                const_("who", str_("Velt")),
                es(log(vec![call("greet", vec![var("who")])])),
                es(log(vec![call("greet", vec![str_("world")])])),
                const_("n", int(42)),
                const_("pi", float(3.5)),
                es(log(vec![tpl(
                    &["n=", " pi=", " ok=", " sum=", ""],
                    vec![
                        var("n"),
                        var("pi"),
                        bin(B::Gt, var("n"), int(40)),
                        bin(B::Add, var("n"), int(1)),
                    ],
                )])),
                let_("s", str_("a")),
                es(assign(var("s"), bin(B::Add, var("s"), str_("b")))),
                es(cassign(B::Add, var("s"), str_("c"))),
                es(log(vec![var("s"), member(var("s"), "length")])),
                es(log(vec![
                    bin(B::Eq, str_("abc"), var("s")),
                    bin(B::NotEq, str_("abd"), var("s")),
                    bin(B::Lt, str_("abc"), str_("abd")),
                ])),
                es(log(vec![tpl(&["multi\nline"], vec![])])),
                es(log(vec![str_("esc: \"q\" \\ \t|")])),
                for_(
                    Some(let_("i", int(0))),
                    Some(bin(B::Lt, var("i"), int(3))),
                    Some(post_inc(var("i"))),
                    vec![
                        const_(
                            "line",
                            tpl(
                                &["row ", ": ", ""],
                                vec![var("i"), call("greet", vec![var("who")])],
                            ),
                        ),
                        es(log(vec![var("line")])),
                    ],
                ),
            ],
        ),
    ]
}

// ───────────────────────────── golden programs ─────────────────────────────

#[test]
fn golden_hello() {
    let p = ok(hello());
    let main = fn_named(&p, "main");
    assert_eq!(
        p.entry,
        Some(
            p.defs
                .iter()
                .position(|d| matches!(d, hir::Def::Fn(f) if f.name == "main"))
                .map(|i| hir::DefId(i as u32))
                .unwrap()
        )
    );
    assert_eq!(*p.types.kind(main.ret), TyKind::Unit);
    let e = exprs_of(main);
    assert!(matches!(
        e[0].kind,
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Print),
            ..
        }
    ));
}

#[test]
fn golden_arith() {
    let p = ok(arith());
    let main = fn_named(&p, "main");
    let tys: Vec<_> = main
        .body
        .locals
        .iter()
        .map(|l| (l.name.as_str(), p.types.kind(l.ty).clone()))
        .collect();
    assert!(tys.contains(&("a", TyKind::Float(hir::FloatTy::F64))));
    assert!(tys.contains(&("x", TyKind::Float(hir::FloatTy::F64))));
    assert!(tys.contains(&("small", TyKind::Int(hir::IntTy::I32))));
    assert!(tys.contains(&("u", TyKind::Int(hir::IntTy::U8))));
    // `u + 5`: the literal takes u8; `300 as u8`: the literal is i64, then cast.
    let exprs = exprs_of(main);
    let u8_lits = exprs
        .iter()
        .filter(|e| {
            matches!(e.kind, E::Lit(hir::Lit::Int(5)))
                && *p.types.kind(e.ty) == TyKind::Int(hir::IntTy::U8)
        })
        .count();
    assert_eq!(u8_lits, 1);
    assert!(exprs.iter().any(|e| matches!(&e.kind, E::Cast(inner)
        if matches!(inner.kind, E::Lit(hir::Lit::Int(300))) && *p.types.kind(inner.ty) == TyKind::Int(hir::IntTy::I64))
        && *p.types.kind(e.ty) == TyKind::Int(hir::IntTy::U8)));
    // `!(a <= b) && true || false` → Logical.
    assert!(exprs.iter().any(|e| matches!(
        e.kind,
        E::Logical {
            op: hir::LogicOp::Or,
            ..
        }
    )));
    // All reads are Copy.
    assert!(uses(main).iter().all(|(_, m)| *m == UseMode::Copy));
}

#[test]
fn golden_control() {
    let p = ok(control());
    let main = fn_named(&p, "main");
    let stmts = &main.body.block.stmts;
    // for (let j = 0; ...; j++) → Block { Let j; While { step: CompoundAssign } }
    let for_j = stmts.iter().find_map(|s| match &s.kind {
        S::Block(b) => Some(b),
        _ => None,
    });
    let b = for_j.expect("desugared for");
    assert!(matches!(b.stmts[0].kind, S::Let { .. }));
    match &b.stmts[1].kind {
        S::While {
            label: None,
            step: Some(step),
            ..
        } => {
            assert!(matches!(
                step.kind,
                E::CompoundAssign {
                    op: hir::BinOp::Add,
                    ..
                }
            ))
        }
        other => panic!("expected while, got {other:?}"),
    }
    // do-while (no continue) → While { true, { {body}; if (!c) break; } }
    let dw = stmts
        .iter()
        .find_map(|s| match &s.kind {
            S::While {
                cond,
                body,
                step: None,
                ..
            } if matches!(cond.kind, E::Lit(hir::Lit::Bool(true))) => Some(body),
            _ => None,
        })
        .expect("do-while");
    assert!(matches!(dw.stmts[0].kind, S::Block(_)));
    assert!(
        matches!(&dw.stmts[1].kind, S::If { then, .. } if matches!(then.stmts[0].kind, S::Break(None)))
    );
    // labeled outer for → While with label "outer"
    let labeled = stmts.iter().rev().find_map(|s| match &s.kind {
        S::Block(b) => b.stmts.iter().find_map(|s| match &s.kind {
            S::While { label: Some(l), .. } => Some(l.clone()),
            _ => None,
        }),
        _ => None,
    });
    assert_eq!(labeled.as_deref(), Some("outer"));
    // ternary → If expr of type string
    let exprs = exprs_of(main);
    assert!(exprs
        .iter()
        .any(|e| matches!(e.kind, E::If { .. }) && *p.types.kind(e.ty) == TyKind::Str));
    // console.log(kind) borrows the string
    assert!(uses(main).contains(&("kind".to_string(), UseMode::Borrow)));
}

#[test]
fn golden_functions() {
    let p = ok(functions());
    let main = fn_named(&p, "main");
    assert_eq!(*p.types.kind(main.ret), TyKind::Int(hir::IntTy::I32));
    let fib = fn_named(&p, "fib");
    assert_eq!(fib.params.len(), 1);
    assert_eq!(fib.params[0].mode, hir::PassMode::Copy);
    assert!(!fib.body.locals[0].mutable, "fib never assigns its param");
    // Forward reference: isEven calls isOdd (defined later).
    let is_even = fn_named(&p, "isEven");
    assert!(exprs_of(is_even).iter().any(|e| matches!(
        e.kind,
        E::Call {
            callee: Callee::Def(..),
            ..
        }
    )));
    // gcd reassigns its params.
    let gcd = fn_named(&p, "gcd");
    assert!(exprs_of(gcd)
        .iter()
        .any(|e| matches!(e.kind, E::Assign { .. })));
    assert!(
        gcd.body.locals[0].mutable,
        "assigned params are recorded as mutable"
    );
}

#[test]
fn golden_strings() {
    let p = ok(strings());
    let greet = fn_named(&p, "greet");
    assert_eq!(greet.params[0].mode, hir::PassMode::Borrow);
    assert!(!greet.body.locals[0].mutable);
    // `Hello, ${name}!` → StrConcat(StrConcat("Hello, ", name), "!"), name borrowed.
    let S::Return(Some(r)) = &greet.body.block.stmts[0].kind else {
        panic!()
    };
    let E::Call {
        callee: Callee::Intrinsic(Intrinsic::StrConcat),
        args,
    } = &r.kind
    else {
        panic!("{r:?}")
    };
    assert!(matches!(&args[1].kind, E::Lit(hir::Lit::Str(s)) if s == "!"));
    let E::Call {
        callee: Callee::Intrinsic(Intrinsic::StrConcat),
        args: inner,
    } = &args[0].kind
    else {
        panic!()
    };
    assert!(matches!(&inner[0].kind, E::Lit(hir::Lit::Str(s)) if s == "Hello, "));
    assert!(matches!(inner[1].kind, E::Local(_, UseMode::Borrow)));

    let main = fn_named(&p, "main");
    let exprs = exprs_of(main);
    // Non-string template parts are wrapped in ToString (n, pi, n > 40, n + 1, i) = 5.
    let to_strings = exprs
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                E::Call {
                    callee: Callee::Intrinsic(Intrinsic::ToString),
                    ..
                }
            )
        })
        .count();
    assert_eq!(to_strings, 5);
    // s += "c" → Assign { s, StrConcat(s, "c") }
    assert!(exprs
        .iter()
        .any(|e| matches!(&e.kind, E::Assign { value, .. }
        if matches!(value.kind, E::Call { callee: Callee::Intrinsic(Intrinsic::StrConcat), .. }))));
    // s.length → StrLen : usize
    assert!(exprs.iter().any(|e| matches!(
        e.kind,
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::StrLen),
            ..
        }
    ) && *p.types.kind(e.ty) == TyKind::Int(hir::IntTy::USize)));
    // `greet(who)` borrows; nothing is moved in main.
    let u = uses(main);
    assert!(u.contains(&("who".to_string(), UseMode::Borrow)));
    assert!(!u.iter().any(|(_, m)| *m == UseMode::Move));
    // String comparison stays a Binary op on Str operands.
    assert!(exprs.iter().any(|e| matches!(&e.kind, E::Binary { op: hir::BinOp::Lt, lhs, .. } if *p.types.kind(lhs.ty) == TyKind::Str)));
}

// ───────────────────────────── error goldens ─────────────────────────────

/// Compare the first error against `errors/<name>.err` (`file:line:col` + message).
fn check_err(name: &str, src: &Src, items: Vec<velt_syntax::ast::Item>) {
    let d = errs(items);
    let err = std::fs::read_to_string(golden_dir().join(format!("errors/{name}.err"))).unwrap();
    let lines: Vec<&str> = err
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let pos: Vec<&str> = lines[0].rsplitn(3, ':').collect();
    let (line, col): (usize, usize) = (pos[1].parse().unwrap(), pos[0].parse().unwrap());
    let msg = lines[1].trim_start_matches("error: ");
    assert_eq!(
        d.len(),
        1,
        "{name}: expected exactly one diagnostic, got {:#?}",
        d
    );
    assert!(
        d[0].message.contains(msg),
        "{name}: `{}` does not contain `{msg}`",
        d[0].message
    );
    assert_eq!(
        d[0].labels[0].span.lo,
        src.offset(line, col),
        "{name}: wrong position"
    );
}

#[test]
fn error_undefined() {
    let src = Src::load("errors/undefined.vlt");
    let y = src.sp("log(y", 0);
    let y = velt_common::Span::new(y.file, y.lo + 4, y.lo + 5);
    let items = vec![func("main", &[], None, vec![es(log(vec![var("y").at(y)]))])];
    check_err("undefined", &src, items);
}

/// Strings are values: `const b = a` with `a` used afterwards copies (`Clone` of a borrow);
/// the last use of `b` is still a plain borrow.
#[test]
fn string_copy_keeps_the_source() {
    let items = vec![func(
        "main",
        &[],
        None,
        vec![
            const_("a", tpl(&["x", ""], vec![int(1)])),
            const_("b", var("a")),
            es(log(vec![var("a"), var("b")])),
        ],
    )];
    let p = ok(items);
    let main = fn_named(&p, "main");
    let clones = exprs_of(main)
        .into_iter()
        .filter(|e| {
            matches!(&e.kind, E::Call { callee: Callee::Intrinsic(Intrinsic::Share), args }
                if matches!(args[0].kind, E::Local(_, UseMode::Borrow)))
        })
        .count();
    assert_eq!(clones, 1);
}

#[test]
fn error_type_mismatch() {
    let src = Src::load("errors/type_mismatch.vlt");
    let init = str_("hello").at(src.sp("\"hello\"", 0));
    let items = vec![func("main", &[], None, vec![const_t("x", "i64", init)])];
    let d = errs(items.clone());
    assert_eq!(d[0].notes, vec!["expected i64, found string".to_string()]);
    check_err("type_mismatch", &src, items);
}

#[test]
fn error_const_assign() {
    let src = Src::load("errors/const_assign.vlt");
    let asg = assign(var("x").at(src.sp("x = 2", 0)), int(2)).at(src.sp("x = 2", 0));
    let items = vec![func("main", &[], None, vec![const_("x", int(1)), es(asg)])];
    check_err("const_assign", &src, items);
}

// ───────────────────────────── real parser (enable after merge) ─────────────────────────────

#[test]
fn goldens_with_real_parser() {
    use velt_common::FileId;
    let dir = golden_dir();
    let mut failures = vec![];
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.extend(
        std::fs::read_dir(dir.join("errors"))
            .unwrap()
            .flatten()
            .map(|e| e.path()),
    );
    files.sort();
    for f in files
        .iter()
        .filter(|f| f.extension().is_some_and(|e| e == "vlt"))
    {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if name == "parse.vlt" {
            continue;
        }
        let text = std::fs::read_to_string(f).unwrap().replace("\r\n", "\n");
        let (ast, pd) = velt_syntax::parse_file(FileId(0), &text);
        if !pd.is_empty() {
            failures.push(format!(
                "{name}: parse errors: {:?}",
                pd.iter().map(|d| &d.message).collect::<Vec<_>>()
            ));
            continue;
        }
        let m = velt_sema::SourceModule {
            path: "main".into(),
            is_std: false,
            file: FileId(0),
            ast,
            imports: vec![],
            jsx_runtime: None,
        };
        let (p, d) = velt_sema::check(&[m], 0);
        let err_file = f.with_extension("err");
        if err_file.exists() {
            let src = Src { text };
            let err = std::fs::read_to_string(&err_file).unwrap();
            let lines: Vec<&str> = err
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let pos: Vec<&str> = lines[0].rsplitn(3, ':').collect();
            let off = src.offset(pos[1].parse().unwrap(), pos[0].parse().unwrap());
            let msg = lines[1].trim_start_matches("error: ");
            if p.is_some()
                || !d
                    .iter()
                    .any(|d| d.message.contains(msg) && d.labels[0].span.lo == off)
            {
                failures.push(format!(
                    "{name}: expected `{msg}` at {off}, got {:?}",
                    d.iter()
                        .map(|d| (&d.message, d.labels[0].span.lo))
                        .collect::<Vec<_>>()
                ));
            }
        } else if p.is_none() || d.iter().any(|d| d.is_error()) {
            failures.push(format!(
                "{name}: {:?}",
                d.iter().map(|d| &d.message).collect::<Vec<_>>()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

//! `JSON.stringify` / `JSON.parse<T>` glue on hand-built HIR: the `tests/golden/m4/json.vlt`
//! lines that use the intrinsics (the `json.Value` lines are std externs), failure messages
//! with paths, duplicate/unknown keys, nested structs, classes, optional fields, narrow ints,
//! and no leaks on any failure path (the interpreter checks every run).

use velt_sema::hir::{AdtKind, Def, IntTy, TyId, TyKind, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::programs_m4::{json_golden, json_types, null_default, parse, stringify, Json};
use super::run;

/// `try { const v = JSON.parse<T>(src); <ok(v)> } catch (e) { console.log("json error:", e.message) }`
fn parse_or_report(
    f: &mut FB,
    j: &Json,
    src: &str,
    ty: TyId,
    t: T,
    ok: impl FnOnce(&FB, velt_sema::hir::LocalId) -> Vec<velt_sema::hir::Stmt>,
) -> velt_sema::hir::Stmt {
    let v = f.local("v", ty);
    let e = f.local("e", j.err);
    let mut body = vec![let_(v, parse(src, ty, t))];
    body.extend(ok(f, v));
    let msg = field(f.bw(e), 0, U::Borrow, t.str);
    try_(
        body,
        Some((Some(e), vec![se(print(vec![s("json error:", t), msg], t))])),
        None,
    )
}

#[test]
fn json_golden_lines() {
    let out = run(&json_golden());
    let expected = "{\"name\":\"ann\",\"age\":30,\"tags\":[\"a\",\"b\"]}\n\
                    bob 41 0 b@x.io\n\
                    json error: expected string at $.name\n\
                    [1,2,3] \"q\\\"uote\"\n";
    assert_eq!(out.stdout, expected);
    assert_eq!(out.code, 0);
}

/// Error messages and cleanup: every failure after partially decoded owned values.
#[test]
fn json_parse_failures_and_keys() {
    let mut pb = PB::new();
    let t = pb.t;
    let j = json_types(&mut pb);
    let user_ty = j.user_ty;
    let mut f = FB::new("main", t.unit);
    let show = |f: &FB, v| {
        vec![se(print(
            vec![
                field(f.bw(v), 0, U::Borrow, t.str),
                field(f.bw(v), 1, U::Copy, t.i64),
            ],
            t,
        ))]
    };
    let cases = [
        r#"{"tags":["a","b"],"name":"x"}"#,
        r#"{"name":"x","age":1,"tags":["a",2]}"#,
        r#"{"tags":["a"],"name":"x","age":"no"}"#,
        r#"[1]"#,
        r#"{"name":"a","name":"b","extra":{"x":[1,{"y":null}],"s":"\n"},"age":5,"tags":["t"]}"#,
        r#"{"name":"c","age":7,"tags":[],"email":null} "#,
        r#"{"name":"d","age":8,"tags":[]} x"#,
        r#"{"name":"e","age":9.5,"tags":[]}"#,
    ];
    let body = cases
        .iter()
        .map(|src| parse_or_report(&mut f, &j, src, user_ty, t, show))
        .collect();
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    let expected = [
        "json error: expected field \"age\" at $",
        "json error: expected string at $.tags[1]",
        "json error: expected i64 at $.age",
        "json error: expected object at $",
        "b 5",
        "c 7",
        "json error: invalid JSON at $: unexpected trailing characters (byte 31)",
        "json error: expected i64 at $.age",
    ];
    assert_eq!(out.stdout, expected.join("\n") + "\n");
}

/// Nested structs, a class with an array of structs, optional fields present and absent,
/// narrow ints (range-checked), floats, bools; stringify ↔ parse round trip.
#[test]
fn json_nested_classes_and_optionals() {
    let mut pb = PB::new();
    let t = pb.t;
    let e = pb.add_def(Def::Adt(adt(
        "JsonError",
        AdtKind::Struct,
        vec![("message", t.str, None)],
    )));
    let err = pb.adt_ty(e, vec![]);
    let u8t = pb.ty(TyKind::Int(IntTy::U8));
    let of = pb.opt(t.f64);
    let p = pb.add_def(Def::Adt(adt(
        "P",
        AdtKind::Struct,
        vec![("x", t.i64, None), ("y", t.f64, None)],
    )));
    let pt = pb.adt_ty(p, vec![]);
    let item = pb.add_def(Def::Adt(adt(
        "Item",
        AdtKind::Struct,
        vec![
            ("id", u8t, None),
            ("ok", t.bool, None),
            ("w", of, null_default(of)),
            ("at", pt, None),
        ],
    )));
    let it = pb.adt_ty(item, vec![]);
    let ia = pb.arr(it);
    let bx = pb.add_def(Def::Adt(adt(
        "Box",
        AdtKind::Class,
        vec![("items", ia, None), ("label", t.str, None)],
    )));
    let bt = pb.adt_ty(bx, vec![]);
    let mut f = FB::new("main", t.unit);
    f.throws = Some(err);
    let b = f.local("b", bt);
    let src = r#"{"label":"L","items":[{"id":1,"ok":true,"at":{"x":-2,"y":0.5}},{"at":{"y":1e3,"x":7},"w":2.25,"ok":false,"id":255}]}"#;
    let j = Json {
        user: p,
        user_ty: pt,
        err,
    };
    let body = vec![
        let_(b, parse(src, bt, t)),
        se(print(vec![stringify(f.bw(b), t)], t)),
        parse_or_report(
            &mut f,
            &j,
            r#"{"label":"L","items":[{"id":256,"ok":true,"at":{"x":1,"y":2}}]}"#,
            bt,
            t,
            |_, _| vec![],
        ),
        parse_or_report(
            &mut f,
            &j,
            r#"{"label":"L","items":[{"id":1,"ok":1,"at":{"x":1,"y":2}}]}"#,
            bt,
            t,
            |_, _| vec![],
        ),
        parse_or_report(
            &mut f,
            &j,
            r#"{"label":"L","items":[{"id":1,"ok":true,"at":{"x":1}}]}"#,
            bt,
            t,
            |_, _| vec![],
        ),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    let expected = [
        r#"{"items":[{"id":1,"ok":true,"at":{"x":-2,"y":0.5}},{"id":255,"ok":false,"w":2.25,"at":{"x":7,"y":1000}}],"label":"L"}"#,
        "json error: expected u8 at $.items[0].id",
        "json error: expected boolean at $.items[0].ok",
        "json error: expected field \"y\" at $.items[0].at",
    ];
    assert_eq!(out.stdout, expected.join("\n") + "\n");
}

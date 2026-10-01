//! HIR programs shared by the interpreter tests and the native tests (`tests/native.rs`): the
//! intrinsic lines of the M4 golden `json.vlt`.

use velt_sema::hir::{AdtKind, Def, DefId, Expr, Intrinsic as I, Lit, Program, TyId, UseMode as U};

use super::builder::*;
use super::builder_m2::*;

pub(super) struct Json {
    pub user: DefId,
    pub user_ty: TyId,
    pub err: TyId,
}

pub(super) fn null_default(ty: TyId) -> Option<Expr> {
    Some(ex(velt_sema::hir::ExprKind::Lit(Lit::Null), ty))
}

/// `struct User { name: string; age: i64; tags: string[]; email?: string; }` and the
/// prelude's `struct JsonError { message: string }`.
pub(super) fn json_types(pb: &mut PB) -> Json {
    let t = pb.t;
    let sa = pb.arr(t.str);
    let os = pb.opt(t.str);
    let user = pb.add_def(Def::Adt(adt(
        "User",
        AdtKind::Struct,
        vec![
            ("name", t.str, None),
            ("age", t.i64, None),
            ("tags", sa, None),
            ("email", os, null_default(os)),
        ],
    )));
    let user_ty = pb.adt_ty(user, vec![]);
    let e = pb.add_def(Def::Adt(adt(
        "JsonError",
        AdtKind::Struct,
        vec![("message", t.str, None)],
    )));
    let err = pb.adt_ty(e, vec![]);
    Json { user, user_ty, err }
}

pub(super) fn stringify(e: Expr, t: T) -> Expr {
    intr(I::JsonStringify, vec![e], t.str)
}

pub(super) fn parse(src: &str, ty: TyId, t: T) -> Expr {
    intr(I::JsonParse, vec![s(src, t)], ty)
}

pub(super) fn json_golden() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let j = json_types(&mut pb);
    let (sa, os, ia) = (pb.arr(t.str), pb.opt(t.str), pb.arr(t.i64));
    let mut f = FB::new("main", t.unit);
    f.throws = Some(j.err);
    let u = f.local("u", j.user_ty);
    let back = f.local("back", j.user_ty);
    let e = f.local("e", j.err);
    let lit = adt_lit(
        j.user,
        vec![
            s("ann", t),
            int(30, t.i64),
            array(vec![s("a", t), s("b", t)], sa),
            null(os),
        ],
        j.user_ty,
    );
    let body = vec![
        let_(u, lit),
        se(print(vec![stringify(f.bw(u), t)], t)),
        let_(
            back,
            parse(
                r#"{"name":"bob","age":41,"tags":[],"email":"b@x.io"}"#,
                j.user_ty,
                t,
            ),
        ),
        se(print(
            vec![
                field(f.bw(back), 0, U::Borrow, t.str),
                field(f.bw(back), 1, U::Copy, t.i64),
                intr(
                    I::ArrayLen,
                    vec![field(f.bw(back), 2, U::Borrow, sa)],
                    t.usize,
                ),
                unwrap_some(field(f.bw(back), 3, U::Borrow, os), U::Borrow, t.str),
            ],
            t,
        )),
        try_(
            vec![se(parse(
                r#"{"name": 1, "age": 2, "tags": []}"#,
                j.user_ty,
                t,
            ))],
            Some((
                Some(e),
                vec![se(print(
                    vec![s("json error:", t), field(f.bw(e), 0, U::Borrow, t.str)],
                    t,
                ))],
            )),
            None,
        ),
        se(print(
            vec![
                stringify(array((1..=3).map(|v| int(v, t.i64)).collect(), ia), t),
                stringify(s("q\"uote", t), t),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

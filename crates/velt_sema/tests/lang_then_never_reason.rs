//! The reason a `then` or `catch` handler gets from a promise that never rejects is `never`
//! (#3924): `later`'s `Promise<number>` has an error-set variable, solved empty, and its
//! handlers are typed with the set assumed empty (`promise_then.rs`). A cast of the reason
//! (`e as Error`) is a `never` value, as in a check without variables (`coerce_as_written`),
//! and reading the `never` local is unreachable code (velt_vir `local_expr`).

mod common;

use common::programs::ok_src;
use velt_sema::hir::{self, Def, TyKind};

const SRC: &str = r#"
function later(n: number): Promise<number> {
  return new Promise((r) => setTimeout(() => r(n), 1));
}
async function main(): Promise<void> {
  const s: Promise<void> = later(1).then((v) => { console.log("v", v); }, (e) => { const x = e as Error; });
  await s;
  let m = "none";
  await later(2).then((v) => { console.log("v", v); }, (e) => { m = (e as Error).message; });
  await later(3).catch((e) => { const x = e as Error; return 0; });
  console.log(m);
}
main();
"#;

#[test]
fn handlers_of_a_promise_that_never_rejects_get_a_never_reason() {
    let p = ok_src(SRC);
    let handlers: Vec<&hir::FnDef> = (p.defs.iter())
        .filter_map(|d| match d {
            Def::Fn(f) if f.name.starts_with("main::{closure") => Some(f),
            _ => None,
        })
        .collect();
    // The reasons (captures are the leading params: the reason is the one named `e`), and the
    // locals the casts of them initialize: `never`, as in a check without variables, not
    // `Error` locals holding a read of nothing.
    let of = |name: &str| -> Vec<&TyKind> {
        (handlers.iter())
            .flat_map(|f| f.body.locals.iter().filter(move |l| l.name == name))
            .map(|l| p.types.kind(l.ty))
            .collect()
    };
    let (reasons, casts) = (of("e"), of("x"));
    assert_eq!(
        (reasons.len(), casts.len()),
        (3, 2),
        "{reasons:?} {casts:?}"
    );
    assert!(
        (reasons.iter().chain(&casts)).all(|k| matches!(k, TyKind::Never)),
        "{reasons:?} {casts:?}"
    );
}

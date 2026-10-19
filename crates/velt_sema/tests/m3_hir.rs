//! Key HIR shapes of the M3/M4 goldens (async functions, awaits, task/shared-state intrinsics,
//! async closures, soft moves, JSON and HTTP glue).

mod common;

use std::path::PathBuf;

use common::hir_walk::{calls, exprs, func, uses_of};
use common::programs::{load_file, ok_src, repo_root};
use velt_sema::hir::{
    Callee, Def, ExprKind as E, FnDef, Intrinsic, PassMode, Program, TyKind, UseMode,
};

fn golden(rel: &str) -> Program {
    let path: PathBuf = repo_root().join("tests/golden").join(rel);
    let l = load_file(&path);
    let (p, d) = l.check();
    p.unwrap_or_else(|| panic!("{rel} failed:\n{}", l.render(&d)))
}

fn intrinsics(f: &FnDef) -> Vec<Intrinsic> {
    calls(f)
        .into_iter()
        .filter_map(|(c, _)| match c {
            Callee::Intrinsic(i) => Some(*i),
            _ => None,
        })
        .collect()
}

fn awaits(f: &FnDef) -> usize {
    exprs(f)
        .into_iter()
        .filter(|e| matches!(e.kind, E::Await(_)))
        .count()
}

/// Closure defs created in `f`.
fn closures<'p>(p: &'p Program, f: &FnDef) -> Vec<&'p FnDef> {
    exprs(f)
        .into_iter()
        .filter_map(|e| match e.kind {
            E::Closure(d) => match p.def(d) {
                Def::Fn(c) => Some(c),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn async_basic_shapes() {
    let p = golden("m3/async_basic.vlt");
    let delayed = func(&p, "delayed");
    assert!(delayed.is_async);
    assert_eq!(
        delayed.ret, delayed.params[1].ty,
        "HIR `ret` of an async fn is `T`"
    );
    assert!(delayed.params.iter().all(|x| x.mode == PassMode::Copy));
    let sum_all = func(&p, "sumAll");
    assert_eq!(
        sum_all.params[0].mode,
        PassMode::Owned,
        "async params are owned"
    );
    assert_eq!(awaits(sum_all), 1);
    let main = func(&p, "main");
    assert!(main.is_async);
    assert!(p.entry.is_some());
    assert_eq!(awaits(main), 5);
    let mut used = intrinsics(main);
    used.extend(intrinsics(func(&p, "threeSleeps")));
    for i in [
        Intrinsic::PromiseAll,
        Intrinsic::Sleep,
        Intrinsic::PerfNow,
        Intrinsic::Spawn,
    ] {
        assert!(used.contains(&i), "{i:?} missing in {used:?}");
    }
}

#[test]
fn tasks_shapes() {
    let p = golden("m3/tasks.vlt");
    let worker = func(&p, "worker");
    assert_eq!(worker.params[0].mode, PassMode::Copy);
    assert_eq!(worker.params[1].mode, PassMode::Owned);
    assert!(intrinsics(worker).contains(&Intrinsic::SharedAdd));
    let main = func(&p, "main");
    let used = intrinsics(main);
    for i in [
        Intrinsic::SharedNew,
        Intrinsic::Clone,
        Intrinsic::Spawn,
        Intrinsic::MutexNew,
        Intrinsic::MutexWith,
        Intrinsic::SharedGet,
    ] {
        assert!(used.contains(&i), "{i:?} missing in {used:?}");
    }
    let tasks: Vec<&FnDef> = closures(&p, main)
        .into_iter()
        .filter(|c| c.is_async)
        .collect();
    assert_eq!(tasks.len(), 1);
    let task = tasks[0];
    assert!(!matches!(p.types.kind(task.ret), TyKind::Promise(..)));
    assert_eq!(task.captures.len(), 1);
    assert_eq!(
        task.captures[0].mode,
        PassMode::Owned,
        "tasks capture by value"
    );
    assert!(intrinsics(task).contains(&Intrinsic::MutexWith));
}

#[test]
fn spawned_closures_may_throw() {
    let p = golden("m3/tcp_echo.vlt");
    let main = func(&p, "main");
    let task = closures(&p, main)
        .into_iter()
        .find(|c| c.is_async)
        .expect("spawned task");
    assert!(
        task.throws.is_some(),
        "the task's I/O errors are its own (uncaught when spawned)"
    );
    assert!(
        main.throws.is_some(),
        "main awaits `listen`/`connect` directly"
    );
    // `TcpListener` is an object (semantics stage 2): the escaping task owns its capture.
    assert_eq!(task.captures[0].mode, PassMode::Owned);
}

#[test]
fn async_call_arguments_used_again_are_cloned() {
    let p = golden("m3/fs.vlt");
    let main = func(&p, "main");
    let modes = uses_of(main, "dir");
    let moves = modes.iter().filter(|m| **m == UseMode::Move).count();
    assert_eq!(moves, 1, "only the last use moves `dir`: {modes:?}");
    let clones = calls(main)
        .into_iter()
        .filter(|(c, a)| {
            matches!(c, Callee::Intrinsic(Intrinsic::Share))
                && matches!(a[0].kind, E::Local(l, UseMode::Borrow) if main.body.locals[l.0 as usize].name == "dir")
        })
        .count();
    assert!(clones >= 2, "{clones}");
    assert!(
        func(&p, "main").throws.is_some(),
        "uncaught IoError propagates"
    );
}

#[test]
fn borrowed_places_passed_to_async_functions_are_cloned() {
    let p = ok_src(
        "async function take(s: string): Promise<number> { return s.length; }
         function start(s: string): Promise<number> { return take(s); }
         async function main() { console.log(await start(\"abc\")); }",
    );
    let start = func(&p, "start");
    assert_eq!(start.params[0].mode, PassMode::Borrow);
    assert!(intrinsics(start).contains(&Intrinsic::Share));
}

#[test]
fn http_server_shapes() {
    let p = golden("m4/http_server.vlt");
    let serve = func(&p, "std/http::serve");
    assert!(intrinsics(serve).contains(&Intrinsic::HttpHandler));
    let main = func(&p, "main");
    let handler = closures(&p, main)
        .into_iter()
        .find(|c| c.is_async)
        .expect("handler");
    let req = handler.params.last().expect("req param");
    assert_eq!(req.mode, PassMode::Owned, "async closure params are owned");
    assert_eq!(handler.captures.len(), 1);
    assert_eq!(handler.captures[0].mode, PassMode::Owned);
}

#[test]
fn json_shapes() {
    let p = golden("m4/json.vlt");
    let parse = func(&p, "std/prelude/json::JSON.parse");
    assert!(intrinsics(parse).contains(&Intrinsic::JsonParse));
    assert!(parse.throws.is_some(), "JSON.parse throws JsonError");
    let main = func(&p, "main");
    let thrown = main.throws.expect("main throws JsonError");
    match p.types.kind(thrown) {
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => assert!(a.name.ends_with("JsonError"), "{}", a.name),
            _ => panic!("not an ADT"),
        },
        k => panic!("{k:?}"),
    }
}

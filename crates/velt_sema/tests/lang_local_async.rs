//! Local async closures (`ownership::local_async`, #208): an async closure that never reaches a
//! thread boundary shares its captures with its calls (`FnDef::shares_captures`) and may modify
//! them; one that may reach one keeps per-call copies, and modifying a capture is an error that
//! names where it leaves its task.

mod common;

use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, Program};

/// `shares_captures` of each async closure created in function `f` (in definition order).
fn local(p: &Program, f: &str) -> Vec<bool> {
    p.defs
        .iter()
        .filter_map(|d| match d {
            Def::Fn(c) if c.is_async && c.name.starts_with(&format!("{f}::{{closure#")) => {
                Some(c.shares_captures)
            }
            _ => None,
        })
        .collect()
}

/// The locals of `f` that live in cells.
fn cells(p: &Program, f: &str) -> Vec<String> {
    p.defs
        .iter()
        .find_map(|d| match d {
            Def::Fn(c) if c.name == f => Some(
                c.body
                    .locals
                    .iter()
                    .filter(|l| l.boxed)
                    .map(|l| l.name.clone())
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn closures_that_stay_on_their_task_are_local() {
    let p = ok_src(
        "class Ctx { n: i64 = 0; }
         function methods(ctx: Ctx): (by: i64) => Promise<i64> {
           return async (by: i64): Promise<i64> => { ctx.n += by; return ctx.n; };
         }
         async function main() {
           let count = 0;
           const bump = async (): Promise<i64> => { count += 1; return count; };
           const fs: (() => Promise<i64>)[] = [];
           fs.push(async (): Promise<i64> => count);
           await Promise.all([bump(), bump()]);
           count = 10;
           const inc = methods(new Ctx());
           console.log(await inc(1), await fs[0](), count);
         }",
    );
    assert_eq!(local(&p, "methods"), vec![true]);
    assert_eq!(local(&p, "main"), vec![true, true]);
    assert_eq!(cells(&p, "main"), vec!["count".to_string()]);
}

#[test]
fn spawned_closures_are_not_local() {
    let p = ok_src(
        "async function later(f: () => Promise<i64>): Promise<i64> { return await spawn(f()); }
         async function main() {
           const n = 1;
           const direct = async (): Promise<i64> => n;
           await spawn(direct());
           await later(async (): Promise<i64> => n + 1);
           await spawn(async (): Promise<i64> => n + 2);
           const kept = async (): Promise<i64> => n + 3;
           console.log(await kept());
         }",
    );
    assert_eq!(local(&p, "main"), vec![false, false, false, true]);
}

#[test]
fn closures_in_crossing_objects_are_not_local() {
    // `Jobs` reaches `spawn`, so a closure of its field's type stored anywhere may too; one of
    // another shape stays local.
    let p = ok_src(
        "class Jobs { run: () => Promise<i64>; constructor(r: () => Promise<i64>) { this.run = r; } }
         class Other { run: (x: i64) => Promise<i64>; constructor(r: (x: i64) => Promise<i64>) { this.run = r; } }
         async function work(j: Jobs): Promise<i64> { return await j.run(); }
         async function main() {
           const k = 2;
           const j = new Jobs(async (): Promise<i64> => k);
           const o = new Other(async (x: i64): Promise<i64> => x * k);
           await spawn(work(j));
           console.log(await o.run(1));
         }",
    );
    assert_eq!(local(&p, "main"), vec![false, true]);
}

#[test]
fn captures_of_a_crossing_closure_cross() {
    let p = ok_src(
        "async function main() {
           const k = 2;
           const inner = async (): Promise<i64> => k;
           await spawn(async (): Promise<i64> => await inner());
         }",
    );
    assert_eq!(local(&p, "main"), vec![false, false]);
}

#[test]
fn modifying_a_capture_of_a_crossing_closure_is_an_error() {
    let r = err_src(
        "async function main() { let total = 0; const t = spawn(async () => { total += 1; }); await t; }",
    );
    assert!(r.contains("modifies captured `total`"), "{r}");
    assert!(r.contains("reaches `spawn` here"), "{r}");
    assert!(r.contains("shared"), "{r}");
    let r = err_src(
        "class Box { f: () => Promise<void>; constructor(f: () => Promise<void>) { this.f = f; } }
         async function run(b: Box) { await b.f(); }
         async function main() {
           let n = 0;
           const b = new Box(async () => { n += 1; });
           await spawn(run(b));
         }",
    );
    assert!(
        r.contains("is stored in a `Box`, which reaches `spawn` here"),
        "{r}"
    );
}

#[test]
fn a_boundary_in_std_is_reported_at_the_users_call() {
    let r = err_src(
        "import { channel } from \"velt:channel\";
         async function main() {
           let n = 0;
           const ch = channel<() => Promise<void>>();
           await ch.send(async () => { n += 1; });
         }",
    );
    assert!(r.contains("is sent on a channel here"), "{r}");
    assert!(!r.contains("channel.vlt"), "{r}");
}

#[test]
fn closures_passed_to_function_values_are_not_local() {
    let r = err_src(
        "async function main() {
           let n = 0;
           const take = (g: () => Promise<void>): void => {};
           take(async () => { n += 1; });
         }",
    );
    assert!(r.contains("passed to a function value"), "{r}");
}

#[test]
fn a_spawned_tasks_result_crosses() {
    let r = err_src(
        "async function make(): Promise<() => Promise<i64>> {
           let n = 0;
           return async (): Promise<i64> => { n += 1; return n; };
         }
         async function main() { const f = await spawn(make()); console.log(await f()); }",
    );
    assert!(r.contains("modifies captured `n`"), "{r}");
    assert!(r.contains("reaches `spawn` here"), "{r}");
}

#[test]
fn generic_parameters_keep_the_callers_types() {
    let r = err_src(
        "class Job { run: () => Promise<void>; constructor(r: () => Promise<void>) { this.run = r; } }
         function wrap<T>(x: T): shared<Mutex<T>> { return shared(new Mutex<T>(x)); }
         async function main() {
           let n = 0;
           const w = wrap<Job>(new Job(async () => { n += 1; }));
         }",
    );
    assert!(
        r.contains("is stored in a `Job`, which is put in a `Mutex` here"),
        "{r}"
    );
}

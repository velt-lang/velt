//! Proxy traps that throw (#1174): the traps of `ProxyHandler` are function types whose errors
//! are inferred from the closures stored into them, and an operation on `any` throws what the
//! `Proxy` method it runs on a proxy throws.

mod common;

use common::hir_walk::func;
use common::programs::{err_src, ok_src};

const TRAPS: &str = "class TrapError extends Error {}
     function make(): any {
       return new Proxy({ a: 1 }, {
         get(t, k, r) { if (k === \"bad\") { throw new TrapError(\"get\"); } return Reflect.get(t, k, r); },
       });
     }";

#[test]
fn a_trap_that_throws_is_accepted_and_the_reads_throw_its_error() {
    let p = ok_src(&format!(
        "{TRAPS}
         function read(p: any): number {{ return p.a as number; }}
         function main() {{ const p = make(); try {{ console.log(read(p)); }} catch (e) {{ console.log(e instanceof TrapError); }} }}"
    ));
    assert!(func(&p, "read").throws.is_some(), "a read runs the `get` trap");
    // The `try` catches it: `main` throws nothing.
    assert!(func(&p, "main").throws.is_none());
}

#[test]
fn each_operation_throws_what_its_own_trap_throws() {
    let p = ok_src(
        "class HasError extends Error {}
         function make(): any {
           return new Proxy({ a: 1 }, {
             has(t, k) { if (k === \"bad\") { throw new HasError(\"has\"); } return Reflect.has(t, k); },
           });
         }
         function test(p: any): boolean { return \"a\" in p; }
         function read(p: any): number { return p.a as number; }
         function main() { const p = make(); try { console.log(test(p), read(p)); } catch (e) {} }",
    );
    assert!(func(&p, "test").throws.is_some(), "`in` runs the `has` trap");
    assert!(func(&p, "read").throws.is_none(), "a read runs no `has` trap");
}

#[test]
fn a_throws_clause_must_allow_a_traps_error() {
    let r = err_src(&format!(
        "{TRAPS}
         function read(p: any): number throws RangeError {{ return p.a as number; }}
         function main() {{ try {{ console.log(read(make())); }} catch (e) {{}} }}"
    ));
    assert!(r.contains("TrapError"), "{r}");
}

#[test]
fn without_a_throwing_trap_operations_on_any_throw_nothing() {
    let p = ok_src(
        "function make(): any { return new Proxy({ a: 1 }, { get(t, k, r) { return Reflect.get(t, k, r); } }); }
         function read(p: any): number { return p.a as number; }
         function main() { console.log(read(make())); }",
    );
    assert!(func(&p, "read").throws.is_none());
}

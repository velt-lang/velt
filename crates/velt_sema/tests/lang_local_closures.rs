//! Closures held in a `const` and only called (`ownership::local_closures`): they capture by
//! reference and do not escape, unless something proves otherwise; `const me = this` names the
//! object by reference (`body::const_borrow`).

mod common;

use common::hir_walk::func;
use common::programs::ok_src;
use velt_sema::hir::{Def, PassMode, PatKind, Program, StmtKind, UseMode};

/// Capture modes of each closure created in function `f` (in definition order).
fn modes(p: &Program, f: &str) -> Vec<Vec<PassMode>> {
    p.defs
        .iter()
        .filter_map(|d| match d {
            Def::Fn(c) if c.name.starts_with(&format!("{f}::{{closure#")) => {
                Some(c.captures.iter().map(|x| x.mode).collect())
            }
            _ => None,
        })
        .collect()
}

/// The first closure of `f` borrows every capture (it was made non-escaping).
fn borrows(p: &Program, f: &str) -> bool {
    let m = modes(p, f);
    !m[0].is_empty() && m[0].iter().all(|m| *m != PassMode::Owned)
}

#[test]
fn constructor_and_method_closures_borrow_this() {
    let p = ok_src(
        "class A {
           items: string[] = [];
           n: number = 0;
           constructor() {
             const add = (s: string) => { this.items.push(s); };
             add(\"a\");
             const size = (): number => this.items.length;
             this.n = size();
           }
           bump(k: number) {
             const step = () => { this.n += k; };
             step();
             step();
           }
         }
         function main() { const a = new A(); a.bump(2); console.log(a.items, a.n); }",
    );
    assert_eq!(modes(&p, "A.constructor")[0], vec![PassMode::BorrowMut]);
    assert_eq!(modes(&p, "A.constructor")[1], vec![PassMode::Borrow]);
    assert_eq!(
        modes(&p, "A.bump")[0],
        vec![PassMode::BorrowMut, PassMode::Copy]
    );
}

#[test]
fn copies_only_what_is_never_assigned() {
    let p = ok_src(
        "function main() {
           const k: number = 2;
           let j: number = 1;
           let n: number = 0;
           const f = (x: number): number => { n += x; return x * k + j; };
           j = 5;
           console.log(f(1), n);
         }",
    );
    assert_eq!(
        modes(&p, "main")[0],
        vec![PassMode::BorrowMut, PassMode::Copy, PassMode::Borrow]
    );
    assert!(!func(&p, "main").body.locals.iter().any(|l| l.boxed));
}

#[test]
fn closures_that_escape_keep_their_captures() {
    let escaping = [
        // Stored, returned, passed on, captured by another closure, reassignable.
        "class B { cb: () => number = () => 0; xs: number[] = [];
           constructor() { const f = (): number => this.xs.length; this.cb = f; } }
         function main() { console.log(new B().cb()); }",
        "function make(): () => number { const xs: number[] = [1]; const f = (): number => xs.length; return f; }
         function main() { console.log(make()()); }",
        "function main() { const xs: number[] = [1]; const f = (): number => xs.length; const fs = [f]; console.log(fs[0]()); }",
        "function apply(g: () => number): number { return g(); }
         function main() { const xs: number[] = [1]; const f = (): number => xs.length; console.log(apply(f)); }",
        "function main() { const xs: number[] = [1]; const f = (): number => xs.length; const g = () => f(); console.log(g()); }",
        "function main() { const xs: number[] = [1]; let f = (): number => xs.length; console.log(f()); f = () => 0; }",
        // A closure created inside it captures a variable that is assigned later: the cell for
        // it passes through a closure capturing by value.
        "function main() { let label = \"a\"; const later = () => (): string => label + \"!\";
           label = \"b\"; console.log(later()()); }",
        // Called while a reference into what it captures is held.
        "function main() { const xs: number[] = [1]; const f = (ys: number[]) => xs.length + ys.length; console.log(f(xs)); }",
        "function main() { const xs: number[] = [1, 2]; const grow = () => { xs.push(3); };
           for (const x of xs) { if (x == 1) { grow(); } } console.log(xs); }",
        "function both(a: number[], b: number): number { return a.length + b; }
         function main() { const xs: number[] = [1]; const f = (): number => { xs.push(2); return 1; };
           console.log(both(xs, f())); }",
        "function main() { const xs: number[] = [1, 2]; const f = (): number => { xs.push(3); return 1; };
           xs[0] += f(); console.log(xs); }",
        "function main() { const rows: number[][] = [[1]]; const f = () => { rows.push([2]); };
           const r = rows[0]; f(); console.log(r, rows.length); }",
        // Async functions keep their frames across `await`.
        "async function one(): Promise<number> { return 1; }
         async function main() { const xs: number[] = [1]; const f = (): number => xs.length; await one(); console.log(f()); }",
    ];
    for src in escaping {
        let p = ok_src(src);
        let f = if src.contains("function make") {
            "make"
        } else if src.contains("class B") {
            "B.constructor"
        } else {
            "main"
        };
        assert!(
            modes(&p, f)[0].contains(&PassMode::Owned),
            "should escape: {src}\n{:?}",
            modes(&p, f)
        );
    }
}

#[test]
fn closures_only_called_borrow() {
    let borrowing = [
        "function main() { const xs: number[] = [1]; const f = (): number => xs.length; console.log(f(), f()); }",
        "function main() { const xs: number[] = [1]; const f = (y: number) => xs.length + y;
           for (let i = 0; i < 3; i++) { console.log(f(i)); } }",
        "function main() { const xs: number[] = [1]; const f = (): number => { xs.push(2); return 1; };
           const n = f() + xs.length; console.log(n); }",
        // Captured, then moved away and used by the call: the move becomes a share.
        "function main() { const xs: number[] = [1]; const f = (): number => xs.length; const ys = xs; ys.push(2); console.log(f()); }",
        "class C { v: number = 0; set() { const f = (): number => 4; this.v = f(); } }
         function main() { const c = new C(); c.set(); console.log(c.v); }",
    ];
    for src in borrowing {
        let p = ok_src(src);
        let f = if src.contains("class C") {
            "C.set"
        } else {
            "main"
        };
        let m = modes(&p, f);
        assert!(
            m.is_empty() || m[0].is_empty() || borrows(&p, f),
            "should borrow: {src}\n{m:?}"
        );
    }
}

#[test]
fn const_this_names_the_object_by_reference() {
    let p = ok_src(
        "class N { v: number = 0;
           constructor() { const me = this; me.v = 3; }
           get2(): number { const me = this; return me.v * 2; } }
         function main() { const n = new N(); console.log(n.get2()); }",
    );
    for f in ["N.constructor", "N.get2"] {
        let bound = func(&p, f).body.block.stmts.iter().any(|s| {
            matches!(&s.kind, StmtKind::LetPat { pat, .. }
                if matches!(pat.kind, PatKind::Binding(_, UseMode::Borrow)))
        });
        assert!(bound, "{f}: `me` refers to `this`");
    }
    // Used together with `this` in one call: `me` is a share, so the call may modify both.
    let p = ok_src(
        "class N { v: number = 1;
           link(a: N, b: N) { a.v += b.v; }
           twice(): number { const me = this; this.link(me, this); return me.v; } }
         function main() { console.log(new N().twice()); }",
    );
    let bound = func(&p, "N.twice").body.block.stmts.iter().any(|s| {
        matches!(&s.kind, StmtKind::LetPat { pat, .. }
            if matches!(pat.kind, PatKind::Binding(_, UseMode::Borrow)))
    });
    assert!(!bound);
}

#[test]
fn a_copy_changed_through_a_projection_is_borrowed() {
    // `t[0] = 10` changes `t` after the closure is created: a copy would read 1 + 2.
    let p = ok_src(
        "function main() {
           let t: [number, number] = [1, 2];
           const f = (): number => t[0] + t[1];
           t[0] = 10;
           console.log(f());
           let q: [number, number] = [1, 2];
           const h = (): number => q[1];
           q[1] = 7;
           console.log(h());
         }",
    );
    assert_eq!(
        modes(&p, "main"),
        vec![vec![PassMode::Borrow], vec![PassMode::Borrow]]
    );
    // Changed through a projection inside the closure: borrowed mutably.
    let p = ok_src(
        "function main() {
           let t: [number, number] = [1, 2];
           const f = () => { t[1] += 5; };
           f();
           console.log(t[1]);
         }",
    );
    assert_eq!(modes(&p, "main")[0], vec![PassMode::BorrowMut]);
}

#[test]
fn borrowing_never_rejects_a_program() {
    // Borrowing `this` in `clear` would conflict with the loop over `c.items`, and borrowing
    // it in `add` with the argument `a.items[0]`: the closures and `me` share instead.
    let programs = [
        "class Bag { items: number[] = [10, 30];
           clear(): void { const me = this; me.items = [1]; } }
         function main() { const c = new Bag(); let s: number = 0;
           for (const it of c.items) { c.clear(); s += it; } console.log(s, c.items.length); }",
        "class Pile { items: number[] = [5, 6];
           clear(): void { const wipe = () => { this.items = [2]; }; wipe(); } }
         function main() { const p = new Pile(); let s: number = 0;
           for (const it of p.items) { p.clear(); s += it; } console.log(s); }",
        "class Tags { items: string[] = [];
           add(tag: string): void { const put = () => { this.items.push(tag); }; put(); put(); } }
         function main() { const a = new Tags(); a.items.push(\"x\"); a.add(a.items[0]);
           console.log(a.items.join(\",\")); }",
    ];
    for src in programs {
        let p = ok_src(src);
        assert!(
            modes(&p, "Pile.clear")
                .iter()
                .chain(modes(&p, "Tags.add").iter())
                .all(|m| m.contains(&PassMode::Owned)),
            "should share: {src}"
        );
    }
}

//! Which classes carry a reference count (semantics stage 2: only types a program shares). A
//! closure that never leaves the method or constructor creating it, and `const me = this`,
//! borrow `this`, so they leave the class uncounted (#444); one that is stored keeps sharing it.

mod no_window;
mod test_dir;

/// The counted types lowering settles on for `src` (`VELT_DEBUG_COUNTED=1`).
fn counted(src: &str) -> Vec<String> {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
        .env("VELT_DEBUG_COUNTED", "1")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let line = stderr
        .lines()
        .find_map(|l| l.strip_prefix("velt: counted types:"))
        .unwrap_or_else(|| panic!("no counted types reported:\n{stderr}"));
    line.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn closures_that_stay_in_the_constructor_or_method_leave_the_class_uncounted() {
    let c = counted(
        "class A {
           items: number[] = [];
           n: number = 0;
           constructor() {
             const add = (v: number) => { this.items.push(v); };
             add(1);
             const size = (): number => this.items.length;
             this.n = size();
           }
           bump(k: number) {
             const step = () => { this.n += k; };
             step();
             step();
           }
         }
         class B {
           v: number = 0;
           constructor() { const me = this; me.v = 3; }
           twice(): number { const me = this; me.v *= 2; return me.v; }
         }
         function main() {
           const a = new A();
           a.bump(2);
           const b = new B();
           console.log(a.items, a.n, b.twice());
         }",
    );
    assert!(!c.iter().any(|t| t == "class A" || t == "class B"), "{c:?}");
}

#[test]
fn a_stored_closure_still_shares_this() {
    let c = counted(
        "class A {
           n: number = 0;
           cb: () => number = () => 0;
           constructor() {
             const f = (): number => this.n;
             this.cb = f;
           }
         }
         function main() { const a = new A(); console.log(a.cb()); }",
    );
    assert!(c.iter().any(|t| t == "class A"), "{c:?}");
}

/// A by-reference `const` of a field of an object in a variable a closure shares (a shared
/// cell) holds a share of the value: a call of the closure may replace the field and would free
/// what the binding points to (#819). So the field's class is counted.
#[test]
fn fields_borrowed_from_a_shared_cell_are_counted() {
    let c = counted(
        "class A { v = 1 }
         class O { a = new A() }
         function main() {
           const o = new O();
           const g = (): number => { o.a = new A(); return 0; };
           const keep = o.a;
           o.a.v += g();
           console.log(keep.v);
         }",
    );
    assert!(c.iter().any(|t| t == "class A"), "{c:?}");
}

/// The same borrows of a variable no closure shares stay plain references: nothing counts.
#[test]
fn fields_borrowed_from_a_plain_variable_stay_uncounted() {
    let c = counted(
        "class A { v = 1 }
         class O { a = new A(); xs: A[] = [new A()] }
         function main() {
           const o = new O();
           const keep = o.a;
           for (const x of o.xs) { console.log(x.v); }
           console.log(keep.v);
         }",
    );
    assert!(!c.iter().any(|t| t.starts_with("class")), "{c:?}");
}

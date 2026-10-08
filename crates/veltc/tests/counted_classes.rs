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

//! A program pays for what it can call through `any` (#2424). `new Proxy` loads velt/lang/proxy,
//! whose `AnyCall` slot can reach every generic array algorithm on a proxy; those are compiled only
//! when the program can call one (`anyMethodCalled`), `Reflect.get`'s answer to a class getter read
//! on a brand only when such a read exists (`stdUsed`), and a member of any class only when its name
//! is called (`Cx::any_called`). One `new Proxy` took a program from 59 to 318,901 lines of release
//! IR before; the budget below keeps the IR from growing back unnoticed.

mod no_window;
mod test_dir;

/// The release LLVM IR of `src`, compiled as `main.velt`.
fn release_ir(src: &str) -> String {
    ir_of(src, &["--release"])
}

/// The LLVM IR of `src`, compiled as `main.velt` with `flags`.
fn ir_of(src: &str, flags: &[&str]) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "llvm"])
        .args(flags)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const READ: &str = "const p = new Proxy({ a: 1 } as any, {});\nconsole.log(p.a);\n";

/// Lines of release IR for `READ`: 37,557 when the budget was set. A real need can raise it; a
/// new dependency of `Proxy` on the array algorithms or on `sort` can't get past it.
const READ_BUDGET: usize = 46_000;

#[test]
fn one_proxy_stays_within_its_budget() {
    let lines = release_ir(READ).lines().count();
    assert!(
        lines <= READ_BUDGET,
        "one `new Proxy` is {lines} lines of release IR, over its budget of {READ_BUDGET}"
    );
}

#[test]
fn array_algorithms_only_when_a_method_can_be_called() {
    let ir = release_ir(READ);
    for name in ["proxySplice", "proxySort", "proxySearch", "proxyReverse"] {
        assert!(!ir.contains(name), "`{name}` in a program that calls none");
    }
    let ir = release_ir("const p = new Proxy([2, 1] as any, {});\np.sort();\n");
    assert!(ir.contains("proxySort"), "`p.sort()` needs its algorithm");
    assert!(!ir.contains("proxySplice"), "`p.sort()` isn't `splice`");
    // Read as a value and run by `call`, the method isn't named by a call.
    let ir = release_ir("const p = new Proxy([2, 1] as any, {});\nconst f = p.sort;\nf.call(p);\n");
    assert!(ir.contains("proxySort"), "`f.call(p)` can run any method");
}

#[test]
fn getter_reads_only_with_a_brand_getter() {
    assert!(!release_ir(READ).contains("classGetterValue"));
    let brand = "class C {\n  n: number = 1;\n  get twice(): number {\n    return this.n * 2;\n  }\n}\nconst p: any = new Proxy(new C() as any, {\n  get(t: any, k: any, r: any) {\n    return Reflect.get(t, k, r);\n  },\n});\nconst c = p as C;\nconsole.log(c.twice);\n";
    assert!(release_ir(brand).contains("classGetterValue"));
}

#[test]
fn a_member_only_when_its_name_is_called() {
    // Debug IR: nothing is inlined, so a function that is compiled has its own definition.
    let class = "class C {\n  usedOne(): number {\n    return 1;\n  }\n  otherOne(): number {\n    return 2;\n  }\n}\nconst c: any = new C();\nconsole.log(c.usedOne());\n";
    let ir = ir_of(class, &[]);
    let defined = |name: &str| {
        ir.lines()
            .any(|l| l.starts_with("define ") && l.contains(name))
    };
    assert!(defined("usedOne"), "the called method is compiled");
    assert!(!defined("otherOne"), "the other one isn't");
}

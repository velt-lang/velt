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
    // Small enough to be inlined in release: the debug IR has its definition.
    assert!(
        !ir_of(READ, &[]).contains("proxyFind"),
        "`find` in a program that calls none"
    );
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
    let brand = include_str!("../../../tests/golden/lang/proxy_getter_receiver.velt");
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

/// The string comparisons in the `AnyCall` glue of class `C` of `src`, in debug IR.
fn call_compares(src: &str) -> usize {
    let ir = ir_of(src, &[]);
    let body = ir
        .split("\ndefine ")
        .find(|f| {
            f.lines()
                .next()
                .is_some_and(|l| l.contains("@\"_Ganycall_1C\""))
        })
        .unwrap_or_else(|| panic!("no `AnyCall` glue of `C`"));
    body.lines()
        .take_while(|l| !l.starts_with('}'))
        .filter(|l| l.contains("str_eq"))
        .count()
}

#[test]
fn a_slot_compares_only_the_names_called() {
    let class = "class C {\n  f: () => number = () => 1;\n  g: () => number = () => 2;\n  m(): number {\n    return 3;\n  }\n  n(): number {\n    return 4;\n  }\n}\nconst c: any = new C();\n";
    let one = call_compares(&format!("{class}c.m();\n"));
    assert_eq!(
        one, 1,
        "only `m`: no other member or `Object.prototype` name"
    );
    // Each of a field, a method and an `Object.prototype` name is compared once it is called.
    let field = call_compares(&format!("{class}c.m();\nc.f();\n"));
    assert_eq!(field, 2);
    let method = call_compares(&format!("{class}c.m();\nc.n();\n"));
    assert_eq!(method, 2);
    let proto = call_compares(&format!("{class}c.m();\nc.hasOwnProperty(\"m\");\n"));
    assert_eq!(proto, 2);
}

#[test]
fn a_forwarding_trap_keeps_the_algorithms_out() {
    for trap in [
        "get(t: any, k: any, r: any) {\n    log.push(k);\n    return Reflect.get(t, k, r);\n  }",
        "get: (t: any, k: any) => t[k]",
    ] {
        let src = format!("const log: any[] = [];\nconst p: any = new Proxy([1] as any, {{\n  {trap},\n}});\nconsole.log(p.length, p[0]);\n");
        assert!(!release_ir(&src).contains("proxySort"), "{trap}");
    }
    for trap in [
        "get(t: any, k: any, r: any) {\n    return Reflect.get(t, k.slice(1), r);\n  }",
        "get(t: any, k: any, r: any) {\n    k = \"sort\";\n    return Reflect.get(t, k, r);\n  }",
        "get(t: any, k: any, r: any) {\n    const n = k + \"\";\n    return Reflect.get(t, n, r);\n  }",
    ] {
        let src = format!("const p: any = new Proxy([1] as any, {{\n  {trap},\n}});\nconsole.log(p.length);\n");
        assert!(release_ir(&src).contains("proxySort"), "{trap}");
    }
}

#[test]
fn a_chain_of_calls_takes_no_pass_per_link() {
    let dir = test_dir::TestDir::new();
    let src = include_str!("../../../tests/golden/lang/any_call_chain_long.velt");
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "llvm"])
        .env("VELT_LOWER_PROFILE", "1")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let log = String::from_utf8_lossy(&out.stderr);
    let grew = log
        .lines()
        .filter(|l| l.contains("grew") && l.contains("any_call_keys"));
    assert_eq!(grew.count(), 0, "{log}");
}

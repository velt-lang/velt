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

/// A read through a proxy, printed as the number it is: printing the `any` itself would bring
/// `console.log`'s line breaking, which is the price of printing a dynamic value, not of a
/// `Proxy` ([`PRINTS_ANY`] measures that).
const READ: &str = "const p = new Proxy({ a: 1 } as any, {});\nconsole.log(p.a as number);\n";

/// Lines of release IR for `READ`: 37,557 when the budget was set, 47,165 since std prints
/// numbers (#3154). A proxy's key text needs a float's text (`p[1.5]` reads the key `"1.5"`),
/// and std's shortest printer, about 5,200 lines, is that now. A real build links it prebuilt
/// (velt/lang/hooks/number_shortest, from the build cache or the toolchain), but `--emit llvm`
/// compiles it in. 57,117 since `charAt` and `slice` of non-ASCII text are std's (#2816):
/// `Reflect.get`'s element read of an `any` target may index a string, and the position
/// translation (velt/lang/string/positions, about 5,100 lines) is prebuilt the same way. A real
/// need can raise the budget; a new dependency of `Proxy` on the array algorithms or on `sort`
/// can't get past it.
const READ_BUDGET: usize = 58_000;

#[test]
fn one_proxy_stays_within_its_budget() {
    let lines = release_ir(READ).lines().count();
    assert!(
        lines <= READ_BUDGET,
        "one `new Proxy` is {lines} lines of release IR, over its budget of {READ_BUDGET}"
    );
}

/// A program that prints an `any`, without a proxy.
const PRINTS_ANY: &str = "const a: any = { a: 1 };\nconsole.log(a.a);\n";

/// Lines of release IR for `PRINTS_ANY`: 42,321 when the budget was set. The value may be an
/// object or an array, which `console.log` breaks across lines as Node does, so the program has
/// std's line breaking (#3508): about 25,300 lines, of std/lang/hooks/inspect_layout_parse
/// (18,300), inspect_layout_render (4,200), inspect_layout_width, inspect_layout_container,
/// inspect_layout_pieces and inspect_layout. A real build links it prebuilt
/// (velt/lang/hooks/inspect_layout_text), but `--emit llvm` compiles it in. A program that
/// prints only numbers, strings and booleans has none of it
/// ([`one_proxy_stays_within_its_budget`]).
const PRINTS_ANY_BUDGET: usize = 45_000;

#[test]
fn printing_an_any_stays_within_its_budget() {
    let lines = release_ir(PRINTS_ANY).lines().count();
    assert!(
        lines <= PRINTS_ANY_BUDGET,
        "printing an `any` is {lines} lines of release IR, over its budget of {PRINTS_ANY_BUDGET}"
    );
    // And the line breaking is what a scalar leaves out.
    let scalar = release_ir("const a: any = { a: 1 };\nconsole.log(a.a as number);\n");
    assert!(
        !scalar.contains("inspect__layout"),
        "line breaking for a number"
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

/// The names of the array methods `anyMethodCalled("…")` asks about in `text`.
fn method_names(text: &str) -> std::collections::BTreeSet<String> {
    let key = "anyMethodCalled(\"";
    text.match_indices(key)
        .filter_map(|(at, _)| {
            let rest = &text[at + key.len()..];
            rest.find('"').map(|end| rest[..end].to_string())
        })
        .collect()
}

/// `proxyArrayInvoke` keeps the records of holes (velt/lang/proxy/holes) only in a program that
/// can call an algorithm whose trap calls make them: its list of method names is every name an
/// algorithm of velt/lang/proxy is compiled for. A method left out of it would run its algorithm
/// without the records.
#[test]
fn hole_records_whenever_an_algorithm_can_run() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/lang/proxy");
    let (start, end) = (
        "export function proxyArrayInvoke(",
        "function recordedInvoke(",
    );
    let mut guard = None;
    let mut algorithms = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".velt") || name.ends_with(".test.velt") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let Some(a) = text.find(start) else {
            algorithms.extend(method_names(&text));
            continue;
        };
        let b = a + text[a..]
            .find(end)
            .expect("recordedInvoke after proxyArrayInvoke");
        guard = Some(method_names(&text[a..b]));
        algorithms.extend(method_names(&text[..a]));
        algorithms.extend(method_names(&text[b..]));
    }
    let guard = guard.expect("proxyArrayInvoke in std/lang/proxy");
    assert!(!algorithms.is_empty(), "no algorithm found");
    assert_eq!(
        guard, algorithms,
        "proxyArrayInvoke's list of methods differs from the algorithms'"
    );
    // A program that can call none keeps no records (its `length` read is still a trap call);
    // one that reads a method by a computed name and calls it can call every one.
    // `recordedInvoke` is inlined into `proxyArrayInvoke`; the records' functions are not.
    let ir = release_ir(READ);
    for name in ["methodBegin", "methodEnd"] {
        assert!(!ir.contains(name), "`{name}` for `p.a`");
    }
    let computed = "const p = new Proxy([1, 2] as any, {});\nconst m: string = \
                    [\"po\", \"p\"].join(\"\");\nconsole.log(p[m].call(p) as number);\n";
    assert!(
        release_ir(computed).contains("methodBegin"),
        "`p[m].call(p)` can run any algorithm: it keeps the records"
    );
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
    let program = |handler: &str| {
        format!("const log: any[] = [];\nconst p: any = new Proxy([1] as any, {handler});\nconsole.log(p.length, p[0]);\n")
    };
    for trap in [
        "get(t: any, k: any, r: any) {\n    log.push(k);\n    return Reflect.get(t, k, r);\n  }",
        "get(t: any, k: any, r: any) {\n    const v = Reflect.get(t, k, r);\n    return v;\n  }",
        "get: (t: any, k: any) => t[k]",
    ] {
        let src = program(&format!("{{\n  {trap},\n}}"));
        assert!(!release_ir(&src).contains("proxySort"), "{trap}");
    }
    for trap in [
        "get(t: any, k: any, r: any) {\n    return Reflect.get(t, k.slice(1), r);\n  }",
        "get(t: any, k: any, r: any) {\n    k = \"sort\";\n    return Reflect.get(t, k, r);\n  }",
        "get(t: any, k: any, r: any) {\n    const n = k + \"\";\n    return Reflect.get(t, n, r);\n  }",
        "get(t: any, k: any = \"sort\", r?: any) {\n    return Reflect.get(t, k, r);\n  }",
        "get(t: any, k: any, r: any) {\n    const f = () => Reflect.get(t, k, r);\n    return f();\n  }",
    ] {
        let src = program(&format!("{{\n  {trap},\n}}"));
        assert!(release_ir(&src).contains("proxySort"), "{trap}");
    }
    // Written elsewhere, a handler can be reached by more than the proxy.
    let held = "const h: any = {\n  get(t: any, k: any, r: any) {\n    return Reflect.get(t, k, r);\n  },\n};\n";
    assert!(release_ir(&format!("{held}{}", program("h"))).contains("proxySort"));
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

#[test]
fn a_list_read_makes_no_typed_array_tables() {
    let src = "const u = new Uint8Array(1);\nfunction one(this: any, a: any): any {\n  return this;\n}\nconst f: any = one;\nconsole.log(u.length, f.apply(null, [1]));\n";
    // Debug IR: a table's glue keeps its name, the class is `TypedArray`, and each subclass's
    // members come with it.
    let ir = ir_of(src, &[]);
    let tables = ir
        .lines()
        .filter(|l| l.starts_with("define ") && l.contains("_Gany") && l.contains("TypedArray"));
    assert_eq!(tables.count(), 0, "`any` tables of `TypedArray`");
}

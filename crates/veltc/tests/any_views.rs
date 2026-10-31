//! Views (#1485, velt_vir `lower/any_view.rs`): an `any` holding an object of another type,
//! converted to an object type, becomes a view of it only in a program with typed proxies.
//! A program without proxies keeps the checked conversion and gets no view code at all, also
//! none of the runtime's table that gives an object one view.

mod no_window;
mod test_dir;

/// The VIR of `src`, compiled as `main.vlt`.
fn vir_of(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
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

/// Does a call in `vir` reach a function whose name contains `name`?
fn calls(vir: &str, name: &str) -> bool {
    vir.lines()
        .any(|l| !l.starts_with("fn#") && l.contains("call fn#") && l.contains(name))
}

/// Does a call in `vir` reach the runtime function `name`?
fn calls_rt(vir: &str, name: &str) -> bool {
    vir.lines()
        .any(|l| l.contains("call extern#") && l.contains(name))
}

const CONVERSION: &str = "
type Item = { name: string; price: number };

class Product {
  name = \"pen\";
  price = 2;
  stock = 10;
}

function price(a: any): number {
  const item = a as Item;
  const back: any = item;
  return back === a ? item.price : 0;
}

console.log(price(new Product()));
";

#[test]
fn a_program_with_proxies_makes_views() {
    let src = format!("const tracked = new Proxy({{ hits: 0 }}, {{}});\n{CONVERSION}");
    let vir = vir_of(&src);
    assert!(calls(&vir, "proxyView"), "no call of `Proxy[PROXY_VIEW]`");
    assert!(
        calls(&vir, "proxyIsView"),
        "no view test where a typed proxy goes into an `any`"
    );
    assert!(
        calls_rt(&vir, "velt_rt_view_get") && calls_rt(&vir, "velt_rt_view_put"),
        "no lookup in the table of views where a view is made"
    );
    assert!(
        calls_rt(&vir, "velt_rt_view_remove"),
        "no removal from the table of views where a proxy is dropped"
    );
}

#[test]
fn a_program_without_proxies_has_no_views() {
    let vir = vir_of(CONVERSION);
    assert!(
        !calls(&vir, "proxyView"),
        "a view in a program without proxies"
    );
    assert!(
        !calls(&vir, "proxyIsView"),
        "a view test in a program without proxies"
    );
    assert!(
        !vir.contains("velt_rt_view_"),
        "the table of views in a program without proxies"
    );
}

//! A `ThreadLocal`'s storage is named by the constant's qualified name, not its place among the
//! program's declarations (velt_vir `tls::stable_id`): `velt dev` keeps one value per constant
//! across versions in which other declarations come and go.

mod no_window;
mod test_dir;

/// The `velt_tls_*` symbols of the user's thread-locals in the VIR of `src`, compiled as
/// `main.velt`, sorted: std's own (the stdout mark `console.log` sets, for one) are left out by
/// subtracting what the same calls emit in a program without a `ThreadLocal`.
fn tls_symbols(src: &str) -> Vec<String> {
    static STD: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let std_syms = STD.get_or_init(|| {
        all_tls_symbols("let n: u64 = 0;\nfunction main() {\n  n = n + 1;\n  console.log(n);\n}\n")
    });
    let mut syms = all_tls_symbols(src);
    syms.retain(|s| !std_syms.contains(s));
    syms
}

/// Every `velt_tls_*` symbol in the VIR of `src`, compiled as `main.velt`, sorted.
fn all_tls_symbols(src: &str) -> Vec<String> {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let vir = String::from_utf8_lossy(&out.stdout);
    let mut syms: Vec<String> = vir
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with("velt_tls_"))
        .map(str::to_string)
        .collect();
    syms.sort();
    syms.dedup();
    syms
}

const BODY: &str = "
const hits = new ThreadLocal<u64>(0);

function main() {
  hits.set(hits.get() + 1);
  console.log(hits.get());
}
";

#[test]
fn ids_stay_when_declarations_are_added_before_the_constant() {
    let before = tls_symbols(BODY);
    let more = format!(
        "const limit: u64 = 3;\nfunction helper(): u64 {{ return limit; }}\nclass Box {{ v: u64 = 0; }}\n{BODY}"
    );
    let after = tls_symbols(&more);
    assert_eq!(before.len(), 2, "{before:?}");
    assert_eq!(before, after);
}

#[test]
fn constants_of_different_names_get_different_ids() {
    let renamed = BODY.replace("hits", "misses");
    assert_ne!(tls_symbols(BODY), tls_symbols(&renamed));
}

#[test]
fn a_changed_type_gets_a_different_id() {
    let base = tls_symbols(BODY);
    let key_id = |syms: &[String]| syms[0].split('_').nth(3).map(str::to_string);
    for other in ["ThreadLocal<f64>(0)", "ThreadLocal<i64>(0)"] {
        let changed = tls_symbols(&BODY.replace("ThreadLocal<u64>(0)", other));
        assert_ne!(key_id(&base), key_id(&changed), "{other}: {changed:?}");
    }
}

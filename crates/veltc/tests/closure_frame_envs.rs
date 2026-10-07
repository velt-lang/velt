//! A closure only ever called in the function creating it keeps its environment in the frame,
//! whatever it captures (#34): no heap allocation, no copy or transfer glue. One that is passed
//! on or stored still gets a counted heap environment.

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

/// The body of the VIR function whose header contains `name`.
fn body<'v>(vir: &'v str, name: &str) -> &'v str {
    let start = vir
        .find(&format!("internal {name}("))
        .unwrap_or_else(|| panic!("no function {name} in:\n{vir}"));
    let rest = &vir[start..];
    &rest[..rest.find("\n}\n").unwrap_or(rest.len())]
}

#[test]
fn called_closures_get_a_frame_env() {
    let vir = vir_of(
        "function main() {
           const k: number = 3;
           const scale = (x: number): number => x * k;
           let t: number = 0;
           for (let i: number = 0; i < 4; i++) {
             t += scale(i);
           }
           const s = `x${t}`;
           const len = (): number => s.length;
           console.log(t, len(), ((y: number): number => y * k)(2));
         }",
    );
    assert!(!body(&vir, "_V4main").contains("velt_rt_alloc"), "{vir}");
    assert!(!vir.contains("_Genv_clone_"), "{vir}");
    // The string moved into `len` is dropped with it.
    assert!(vir.contains("_Genv_drop_frame_"), "{vir}");
}

#[test]
fn stored_closures_keep_a_heap_env() {
    let vir = vir_of(
        "function main() {
           const k: number = 3;
           const scale = (x: number): number => x * k;
           const fs = [scale];
           console.log(fs[0](2));
         }",
    );
    assert!(body(&vir, "_V4main").contains("velt_rt_alloc"), "{vir}");
}

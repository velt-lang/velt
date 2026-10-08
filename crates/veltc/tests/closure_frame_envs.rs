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
           const xs: string[] = [s];
           const grow = () => { xs.push(s); };
           for (const x of xs) {
             if (xs.length < 3) { grow(); }
           }
           console.log(t, len(), ((y: number): number => y * k)(2), xs.length);
         }",
    );
    assert!(!vir.contains("_Genv_clone_"), "{vir}");
    // `len` borrows `s` (#444). `grow`, called inside a loop over the array it changes, owns
    // what it captures; that is dropped with it.
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

#[test]
fn borrowing_held_closures_get_a_frame_env() {
    // `step` and `line` borrow `this` and `me` (#444) and are only called (#34).
    let vir = vir_of(
        "class T {
           n: number = 0;
           bump(k: number): number {
             const step = () => { this.n += k; };
             step();
             const me = this;
             const line = (): number => me.n * 2;
             return line();
           }
         }
         function main() { const t = new T(); console.log(t.bump(2)); }",
    );
    assert!(!body(&vir, "_V1TM4bump").contains("velt_rt_alloc"), "{vir}");
    assert!(!vir.contains("_Genv_clone_"), "{vir}");
    assert!(!vir.contains("_Genv_drop_"), "{vir}");
}

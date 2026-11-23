//! The module state's drop at exit with a `weak` module `let` (velt_vir weak_globals.rs): the
//! state tuple's drop glue would drop the slot word as the target object, so a state with a weak
//! slot drops through `_Gmodule_drop`, slot by slot, giving the weak slot up (`weakSlotDrop`) and
//! dropping the others with their glue. It runs at exit, after any check of the program's output
//! or leak counters, so its VIR is what pins it. The function is made only when the module
//! descriptor asks for it.

mod no_window;
mod test_dir;

/// The VIR lowering prints for `src` (`--emit vir`).
fn vir_out(src: &str) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A program with a strong module `let` and one declared `w`'s type.
fn program(w: &str) -> String {
    format!(
        "class N {{ v: number; constructor(v: number) {{ this.v = v; }} }}
         let held: N | null = null;
         let w: {w} = null;
         function set(n: N): void {{ held = n; w = n; }}
         function main() {{ set(new N(1)); console.log(w?.v ?? 0, held?.v ?? 0); }}"
    )
}

/// The body of the function named `name` in `vir`.
fn body<'a>(vir: &'a str, name: &str) -> Option<&'a str> {
    let at = vir
        .lines()
        .position(|l| l.starts_with("fn") && l.contains(name))?;
    let start = vir.lines().take(at).map(|l| l.len() + 1).sum::<usize>();
    let rest = &vir[start..];
    Some(&rest[..rest.find("\n}").map_or(rest.len(), |e| e + 2)])
}

#[test]
fn a_weak_module_let_is_given_up_by_the_state_s_drop() {
    let vir = vir_out(&program("weak N | null"));
    let drop = body(&vir, "_Gmodule_drop").unwrap_or_else(|| panic!("no _Gmodule_drop:\n{vir}"));
    // The strong slot with its drop glue, the weak one's slot given up, and nothing else.
    assert!(drop.contains("_Gdrop_13Option_3cN_3e"), "{drop}");
    assert!(drop.contains("weakSlotDrop"), "{drop}");
    assert_eq!(drop.matches("call ").count(), 2, "{drop}");
}

#[test]
fn a_state_without_one_keeps_its_drop_glue() {
    let vir = vir_out(&program("N | null"));
    assert!(body(&vir, "_Gmodule_drop").is_none(), "{vir}");
    assert!(!vir.contains("weakSlotDrop"), "{vir}");
}

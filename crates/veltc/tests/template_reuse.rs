//! A template literal with fresh parts (calls' results) tries to reuse the longest one's buffer
//! (`velt_vir` `lower/template.rs`, `velt_rt_strbuf_adopt`): one adopt call per fresh part, each
//! reachable. Only allocation counts would show it at run time, so this reads the VIR.

mod no_window;
mod test_dir;

/// The body of function `name` in the VIR dump.
fn body<'v>(vir: &'v str, name: &str) -> &'v str {
    let head = format!("internal _V{}{name}(", name.len());
    let start = vir
        .find(&head)
        .unwrap_or_else(|| panic!("no function {name} in:\n{vir}"));
    let end = start + vir[start..].find("\n}\n").expect("end of function");
    &vir[start..end]
}

#[test]
fn every_fresh_part_can_be_adopted() {
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("main.vlt");
    std::fs::write(
        &src,
        "function page(n: number): string { return \"<p>x</p>\".repeat(n); }\n\
         function doctype(): string { return `<!DOCTYPE html>${page(200)}`; }\n\
         function three(): string { return `${page(1)}|${page(300)}|${page(2)}`; }\n\
         function kept(s: string): string { return `<!DOCTYPE html>${s}`; }\n\
         function main() { console.log(doctype().length, three().length, kept(page(3)).length); }\n",
    )
    .unwrap();
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
    let vir = String::from_utf8_lossy(&out.stdout);
    let adopts = |f: &str| body(&vir, f).matches("velt_rt_strbuf_adopt").count();
    assert_eq!(adopts("doctype"), 1, "{}", body(&vir, "doctype"));
    // Whichever is longest at run time: a dead block for a later part would be pruned.
    assert_eq!(adopts("three"), 3, "{}", body(&vir, "three"));
    // A parameter is not the template's to reuse.
    assert_eq!(adopts("kept"), 0, "{}", body(&vir, "kept"));
}

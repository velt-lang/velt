//! Debug builds call the runtime to share and drop a string instead of writing its test of the
//! string's form out at every use (velt_vir lower/strings.rs `calls_for_size`, #2707). Release
//! builds and `VELT_SHAPES=off` keep the inline form.

mod no_window;
mod test_dir;

/// `--emit vir` of a function that makes a string and drops it, with `env`: its body's lines.
fn body(env: &[(&str, &str)]) -> Vec<String> {
    let dir = test_dir::TestDir::new();
    std::fs::write(
        dir.path().join("main.velt"),
        "function shout(s: string): number {\n\
           const t = s + \"!\";\n\
           return t.length;\n\
         }\n\
         function main() {\n\
           console.log(shout(\"hi\"));\n\
         }\n",
    )
    .unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["build", "--emit", "vir", "main.velt"])
        .current_dir(dir.path());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let vir = String::from_utf8_lossy(&out.stdout).into_owned();
    vir.lines()
        .skip_while(|l| !(l.starts_with("fn#") && l.contains("5shout(")))
        .skip(1)
        .take_while(|l| !l.starts_with("fn#"))
        .map(str::to_string)
        .collect()
}

/// Does `lines` zero a string's three words in place (the inline drop of a string that owns
/// nothing)?
fn zeroes_a_string(lines: &[String]) -> bool {
    lines
        .windows(3)
        .any(|w| (0..3).all(|i| w[i].trim_start().contains(&format!(".{i} = 0_u64"))))
}

#[test]
fn a_debug_build_drops_a_string_with_one_runtime_call() {
    let debug = body(&[]);
    assert!(
        debug.iter().any(|l| l.contains("velt_rt_str_drop")),
        "{debug:#?}"
    );
    assert!(!zeroes_a_string(&debug), "{debug:#?}");
    let inline = body(&[("VELT_SHAPES", "off")]);
    assert!(zeroes_a_string(&inline), "{inline:#?}");
}

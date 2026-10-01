//! Compiling a playground program: source text → browser module (`wasm32-unknown-unknown`),
//! or the rendered diagnostics.
//!
//! The program is the root module of a throwaway build directory. Only `std/…` imports are
//! allowed: relative or package imports would let a visitor read files on the server through
//! the diagnostics' source excerpts.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use velt_common::FileId;
use velt_syntax::ast::ItemKind;

use crate::backend::Backend;
use crate::driver::{self, Artifact, BuildError, BuildOptions, Session};

/// The browser target the playground builds for.
pub const TARGET: &str = "wasm32-unknown-unknown";
/// Largest accepted program.
pub const MAX_SOURCE: usize = 256 * 1024;

/// Why a program did not compile: text for the output pane.
pub type Diagnostics = String;

/// Builds run one at a time: a build uses every core it can get (opt/llc), and a playground
/// serves a handful of people.
static BUILD: Mutex<()> = Mutex::new(());

/// Compile `source` (`optimize`: `--release`) to a module.
pub fn compile(source: &str, optimize: bool) -> Result<Vec<u8>, Diagnostics> {
    check_imports(source)?;
    let dir = scratch_dir().map_err(|e| format!("error: {e}"))?;
    let result = build_in(&dir, source, optimize);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn build_in(dir: &Path, source: &str, optimize: bool) -> Result<Vec<u8>, Diagnostics> {
    let module = dir.join("main.wasm");
    let opts = BuildOptions {
        input: dir.join("main.vlt"),
        root_source: Some(source.to_string()),
        output: Some(module.clone()),
        release: optimize,
        target: Some(TARGET.into()),
        backend: Some(Backend::Llvm),
        ..Default::default()
    };
    let mut sess = Session::new();
    let result = {
        let _one_at_a_time = BUILD.lock().unwrap_or_else(|e| e.into_inner());
        driver::build(&mut sess, &opts)
    };
    // Show `main.vlt:3:5`, not the scratch directory.
    let prefix = format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR);
    let diagnostics = sess.render_diagnostics().replace(&prefix, "");
    match result {
        Ok(Artifact::Executable(_)) => {
            std::fs::read(&module).map_err(|e| format!("error: cannot read the module: {e}"))
        }
        Ok(other) => Err(format!(
            "error: internal: unexpected build output {other:?}"
        )),
        Err(BuildError::Diagnostics) => Err(diagnostics),
        Err(BuildError::Failed(msg)) => Err(format!("{diagnostics}\nerror: {msg}")),
        Err(BuildError::Ice(msg)) => Err(format!("error: internal compiler error: {msg}")),
    }
}

/// Reject imports other than `velt:…` (see the module docs).
fn check_imports(source: &str) -> Result<(), Diagnostics> {
    let (module, _) = velt_syntax::parse_file(FileId(0), source);
    for item in &module.items {
        if let ItemKind::Import(import) = &item.kind {
            if !import.from.starts_with("velt:") {
                return Err(format!(
                    "error: the playground only allows imports from \"velt:…\", not \"{}\"",
                    import.from
                ));
            }
        }
    }
    Ok(())
}

fn scratch_dir() -> Result<PathBuf, String> {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "velt-playground-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_std_imports() {
        assert!(check_imports("import { readFile } from \"velt:fs\";\nfunction main() {}").is_ok());
        for spec in ["./x", "../../etc/passwd", "pkg"] {
            let src = format!("import {{ x }} from \"{spec}\";\nfunction main() {{}}");
            assert!(
                check_imports(&src).unwrap_err().contains("only allows"),
                "{spec}"
            );
        }
    }

    #[test]
    fn diagnostics_name_main_velt() {
        let err = compile("function main( {", false).unwrap_err();
        assert!(err.contains("main.vlt:1:"), "{err}");
        assert!(!err.contains("velt-playground-"), "{err}");
    }
}

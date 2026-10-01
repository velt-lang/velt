//! Whole front end through the driver, as `velt build --emit vir` would run it.

use std::path::PathBuf;
use std::sync::Once;
use veltc::driver::{compile_to_vir, BuildError, BuildOptions, Session};

static STD: Once = Once::new();

/// No panics or ICEs: every input either compiles to verified VIR or yields diagnostics.
pub fn check(src: &str) {
    STD.call_once(|| {
        // The fuzz binary lives under fuzz/target/: point the loader at the repo's std/.
        let std = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../std");
        std::env::set_var("VELT_STD", std);
    });
    let opts = BuildOptions {
        input: PathBuf::from("/virtual/fuzz.vlt"),
        root_source: Some(src.to_string()),
        ..Default::default()
    };
    let mut sess = Session::new();
    if let Err(BuildError::Ice(msg)) = compile_to_vir(&mut sess, &opts) {
        panic!("ICE: {msg}\n--- input\n{src}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn compiles_and_rejects() {
        super::check("function main() {\n  console.log(`${1 + 2}`);\n}\n");
        super::check("function main() { const x: string = 1; }");
    }
}

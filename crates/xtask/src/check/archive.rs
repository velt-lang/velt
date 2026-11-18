//! Packing what the goldens run into one archive and unpacking it (see [`golden_archive`]), so
//! CI builds once per OS and checks the goldens in shards.

use super::*;

impl Gate<'_> {
    /// Packs what the goldens run into `file` (see [`golden_archive`]): for the goldens
    /// (`goldens`), the WebAssembly goldens (`wasm`, after building their runtimes), or both.
    pub(super) fn pack(
        &mut self,
        ws: &[String],
        file: &str,
        goldens: bool,
        wasm: bool,
        identity: bool,
    ) -> Result<(), String> {
        if wasm {
            for (target, _) in golden_archive::WASM_RUNTIMES {
                let mut b = cargo(&["build", "-p", "velt_rt_wasm", "--target", target]);
                self.step(&format!("the WebAssembly runtime ({target})"), &mut b)?;
            }
        }
        let names: Vec<&str> = [
            (goldens, "golden"),
            (wasm, "wasm_goldens"),
            (identity, IDENTITY_TEST),
        ]
        .into_iter()
        .filter_map(|(selected, name)| selected.then_some(name))
        .collect();
        let mut find = cargo(&["test"]);
        find.args(ws);
        for name in &names {
            find.args(["--test", name]);
        }
        find.args(["--no-run", "--message-format=json"]);
        println!("\x1b[36m==> the test binaries\x1b[0m\n    {}", show(&find));
        let mut tests = vec![];
        if self.dry_run {
            tests.extend(
                names
                    .iter()
                    .map(|n| format!("target/debug/deps/{n}-<hash>")),
            );
        } else {
            let out = find
                .current_dir(self.root)
                .output()
                .map_err(|e| format!("cannot run cargo: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "`{}` failed:\n{}",
                    show(&find),
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            let messages = String::from_utf8_lossy(&out.stdout);
            for name in &names {
                let exe = golden_archive::test_binary(&messages, name)
                    .ok_or(format!("cargo built no `{name}` test binary for veltc"))?;
                tests.push(golden_archive::relative(self.root, &exe)?);
            }
        }
        // `tests` is in the order of `names`.
        let binary = |name: &str| {
            let at = names.iter().position(|n| *n == name)?;
            tests.get(at).cloned()
        };
        let manifest = Manifest {
            root: self.root.to_string_lossy().into_owned(),
            test: binary("golden"),
            wasm: binary("wasm_goldens"),
            identity: binary(IDENTITY_TEST),
        };
        if !self.dry_run {
            std::fs::write(self.root.join(MANIFEST), manifest.render())
                .map_err(|e| format!("{MANIFEST}: {e}"))?;
        }
        // Relative paths only: GNU tar (Git's, on Windows) reads `C:` as a remote host.
        let mut tar = Command::new("tar");
        tar.args(["-cf", file, MANIFEST]).args(&tests);
        tar.args(golden_archive::files(|f| {
            self.dry_run || self.root.join(f).is_file()
        }));
        if wasm {
            tar.args(golden_archive::WASM_RUNTIMES.iter().map(|(_, lib)| lib));
        }
        self.step(&format!("pack the goldens' build into {file}"), &mut tar)
    }

    /// Unpacks `file` (made by [`Gate::pack`]) into the checkout; returns its manifest.
    pub(super) fn unpack(&mut self, file: &str) -> Result<Manifest, String> {
        let mut tar = Command::new("tar");
        tar.args(["-xf", file]);
        self.step(&format!("unpack {file}"), &mut tar)?;
        if self.dry_run {
            return Ok(Manifest {
                root: self.root.to_string_lossy().into_owned(),
                test: Some("target/debug/deps/golden-<hash>".into()),
                wasm: Some("target/debug/deps/wasm_goldens-<hash>".into()),
                identity: Some(format!("target/debug/deps/{IDENTITY_TEST}-<hash>")),
            });
        }
        let text = std::fs::read_to_string(self.root.join(MANIFEST))
            .map_err(|e| format!("{file} has no {MANIFEST}: {e}"))?;
        let manifest = Manifest::parse(&text)?;
        let here = self.root.to_string_lossy();
        if manifest.root != here {
            return Err(format!(
                "{file} was built in {}, not in this checkout ({here}): its test binaries have \
                 that checkout's paths compiled in",
                manifest.root
            ));
        }
        Ok(manifest)
    }
}

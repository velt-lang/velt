//! `velt build --host workers`: a Cloudflare Worker (docs/internals/contracts/workers.md). The
//! build writes a directory holding the module, the runtime glue (velt_web.mjs, as every browser
//! build gets, and velt_worker.mjs), the JavaScript of the package host modules the module
//! imports (`hosts/<package>/…`), the Worker's entry `worker.mjs`, and a `wrangler.toml` when
//! there is none, so an edited one is kept. `npx wrangler deploy` in the directory deploys it.

use std::path::{Path, PathBuf};

use vpm::graph::GraphPackage;
use vpm::host_modules::RUNTIME_PREFIX;
use vpm::PackageGraph;

/// The name of the host (`--host workers`).
pub const HOST: &str = "workers";
/// The Worker glue (see the file's header).
const GLUE: &str = include_str!("../../../velt_rt_wasm/js/velt_worker.mjs");
const GLUE_FILE: &str = "velt_worker.mjs";
/// The Worker's entry, which `wrangler.toml` names.
const ENTRY_FILE: &str = "worker.mjs";
/// Where the package host modules go, below the Worker directory.
const HOSTS_DIR: &str = "hosts";
/// The `compatibility_date` a new `wrangler.toml` gets: the Workers runtime the glue was
/// written against.
const COMPATIBILITY_DATE: &str = "2026-10-01";
/// The custom section listing the module's host imports (velt_vir `vir::host::MANIFEST_SECTION`).
const MANIFEST_SECTION: &str = velt_vir::vir::MANIFEST_SECTION;

/// The Worker directory of a build without `-o`: `<dir>/<stem>-worker`.
pub fn default_dir(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}-worker"))
}

/// After `module` was built into its Worker directory: the Worker's other files beside it.
pub fn write(module: &Path, packages: Option<&PackageGraph>) -> Result<(), String> {
    let dir = module.parent().unwrap_or(Path::new("."));
    let bytes =
        std::fs::read(module).map_err(|e| format!("cannot read `{}`: {e}", module.display()))?;
    let mut modules = vec![];
    for name in imported_modules(&bytes)? {
        if name.starts_with(RUNTIME_PREFIX) {
            continue;
        }
        let (package, file) = provider(&name, packages)?;
        let rel = format!(
            "{HOSTS_DIR}/{}/{}",
            package.name,
            file.trim_start_matches("./src/")
        );
        let from = package.root.join(file.trim_start_matches("./"));
        let to = dir.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
        }
        std::fs::copy(&from, &to).map_err(|e| {
            format!(
                "cannot copy host module `{name}` from `{}`: {e}",
                from.display()
            )
        })?;
        modules.push((name, rel));
    }
    let wasm = module.file_name().unwrap_or_default().to_string_lossy();
    write_file(&dir.join(GLUE_FILE), GLUE)?;
    write_file(&dir.join(ENTRY_FILE), &entry(&wasm, &modules))?;
    let config = dir.join("wrangler.toml");
    if !config.exists() {
        let stem = module.file_stem().unwrap_or_default().to_string_lossy();
        write_file(&config, &wrangler_toml(&stem))?;
    }
    Ok(())
}

/// The package providing host module `name`, and its file. The build checked that exactly one
/// package declaring the module provides it (driver `host_modules`).
fn provider<'a>(
    name: &str,
    packages: Option<&'a PackageGraph>,
) -> Result<(&'a GraphPackage, &'a str), String> {
    packages
        .into_iter()
        .flat_map(PackageGraph::packages)
        .find_map(|p| p.host_modules.get(name).map(|f| (p, f.as_str())))
        .ok_or_else(|| format!("no package provides the JavaScript of host module `{name}`"))
}

/// The import modules of the host functions `wasm` imports, from its `velt.host` sections.
fn imported_modules(wasm: &[u8]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for section in custom_sections(wasm, MANIFEST_SECTION)? {
        let text = String::from_utf8_lossy(section);
        for module in text.lines().filter_map(|l| l.split('\t').next()) {
            if !module.is_empty() && !out.iter().any(|m| m == module) {
                out.push(module.to_string());
            }
        }
    }
    Ok(out)
}

/// The contents of the custom sections named `name` of WebAssembly module `wasm`.
fn custom_sections<'a>(wasm: &'a [u8], name: &str) -> Result<Vec<&'a [u8]>, String> {
    let bad = || "the built module is not WebAssembly".to_string();
    if wasm.get(..8) != Some(b"\0asm\x01\0\0\0".as_slice()) {
        return Err(bad());
    }
    let mut out = vec![];
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let size = leb128(wasm, &mut at).ok_or_else(bad)?;
        let end = at
            .checked_add(size)
            .filter(|&e| e <= wasm.len())
            .ok_or_else(bad)?;
        if id == 0 {
            let mut body = at;
            let len = leb128(wasm, &mut body).ok_or_else(bad)?;
            let content = body
                .checked_add(len)
                .filter(|&c| c <= end)
                .ok_or_else(bad)?;
            if &wasm[body..content] == name.as_bytes() {
                out.push(&wasm[content..end]);
            }
        }
        at = end;
    }
    Ok(out)
}

/// An unsigned LEB128 number at `*at` (a `u32` in a module), moving `*at` past it.
fn leb128(bytes: &[u8], at: &mut usize) -> Option<usize> {
    let mut value: usize = 0;
    for shift in (0..35).step_by(7) {
        let b = *bytes.get(*at)?;
        *at += 1;
        value |= usize::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// The Worker's entry: the module, the glue and the host modules (`(name, path)`).
fn entry(wasm: &str, modules: &[(String, String)]) -> String {
    let lit = |s: &str| serde_json::to_string(s).expect("ICE: a string serializes");
    let mut out = String::from(
        "// The Worker's entry, written by `velt build --host workers` (each build writes it again).\n",
    );
    out.push_str(&format!(
        "import module from {};\n",
        lit(&format!("./{wasm}"))
    ));
    out.push_str(&format!(
        "import {{ createWorker }} from {};\n",
        lit(&format!("./{GLUE_FILE}"))
    ));
    for (i, (_, path)) in modules.iter().enumerate() {
        out.push_str(&format!(
            "import host{i} from {};\n",
            lit(&format!("./{path}"))
        ));
    }
    let fields: Vec<String> = modules
        .iter()
        .enumerate()
        .map(|(i, (name, _))| format!("{}: host{i}", lit(name)))
        .collect();
    let modules = match fields.is_empty() {
        true => "{}".to_string(),
        false => format!("{{ {} }}", fields.join(", ")),
    };
    out.push_str(&format!(
        "export default createWorker(module, {{ modules: {modules} }});\n"
    ));
    out
}

/// A new `wrangler.toml` for the Worker named after `stem`.
fn wrangler_toml(stem: &str) -> String {
    format!(
        "# Written by `velt build --host workers` because there was none; later builds keep it.\n\
         name = \"{}\"\nmain = \"{ENTRY_FILE}\"\ncompatibility_date = \"{COMPATIBILITY_DATE}\"\n",
        worker_name(stem)
    )
}

/// A Worker name from a file stem: lowercase letters, digits and `-`, at most 63 characters.
fn worker_name(stem: &str) -> String {
    let name: String = stem
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9') => c,
            _ => '-',
        })
        .take(63)
        .collect();
    let name = name.trim_matches('-');
    if name.is_empty() {
        "velt-worker".into()
    } else {
        name.into()
    }
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A module with custom sections `(name, content)`, a type section between them.
    fn module(sections: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        for (name, content) in sections {
            out.extend([1, 1, 0]);
            let mut body = vec![name.len() as u8];
            body.extend(name.as_bytes());
            body.extend(*content);
            out.push(0);
            // A two-byte size, as LEB128 allows.
            out.extend([(body.len() as u8 & 0x7f) | 0x80, 0]);
            out.extend(body);
        }
        out
    }

    #[test]
    fn host_modules_come_from_the_manifest_sections() {
        let wasm = module(&[
            (
                "velt.host",
                b"kv\tget\tstr\tasync:ref?\nvelt:worker\taccept\t\tasync:ref\n",
            ),
            ("other", b"x\ty\t\tvoid\n"),
            ("velt.host", b"kv\tput\tstr,rbytes\tvoid\napp\tf\t\tvoid\n"),
        ]);
        assert_eq!(
            imported_modules(&wasm).unwrap(),
            ["kv", "velt:worker", "app"]
        );
        assert_eq!(
            imported_modules(&module(&[])).unwrap(),
            Vec::<String>::new()
        );
        assert!(imported_modules(b"\0asm\x02").is_err());
        let mut cut = module(&[("velt.host", b"kv\tget\t\tvoid\n")]);
        cut.truncate(cut.len() - 3);
        assert!(imported_modules(&cut).is_err());
    }

    #[test]
    fn the_worker_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("kv");
        std::fs::create_dir_all(pkg.join("src/js")).unwrap();
        std::fs::write(pkg.join("src/js/kv.mjs"), "export default () => ({});\n").unwrap();
        let mut graph = PackageGraph::default();
        graph
            .add("kv", &pkg, BTreeMap::new())
            .host_modules
            .insert("kv".into(), "./src/js/kv.mjs".into());
        let dir = default_dir(&tmp.path().join("target"), "My_App");
        std::fs::create_dir_all(&dir).unwrap();
        let wasm = dir.join("My_App.wasm");
        let section: &[u8] = b"kv\tget\tstr\tstr\nvelt:worker\taccept\t\tasync:ref\n";
        std::fs::write(&wasm, module(&[("velt.host", section)])).unwrap();
        write(&wasm, Some(&graph)).unwrap();
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        assert_eq!(
            read("worker.mjs"),
            "// The Worker's entry, written by `velt build --host workers` (each build writes it again).\n\
             import module from \"./My_App.wasm\";\n\
             import { createWorker } from \"./velt_worker.mjs\";\n\
             import host0 from \"./hosts/kv/js/kv.mjs\";\n\
             export default createWorker(module, { modules: { \"kv\": host0 } });\n"
        );
        assert!(read("velt_worker.mjs").contains("export function createWorker"));
        assert_eq!(read("hosts/kv/js/kv.mjs"), "export default () => ({});\n");
        assert!(read("wrangler.toml").contains(
            "name = \"my-app\"\nmain = \"worker.mjs\"\ncompatibility_date = \"2026-10-01\"\n"
        ));
        // An edited wrangler.toml is kept.
        std::fs::write(dir.join("wrangler.toml"), "name = \"mine\"\n").unwrap();
        write(&wasm, Some(&graph)).unwrap();
        assert_eq!(read("wrangler.toml"), "name = \"mine\"\n");
        // A module no package provides (the build reports it first).
        let err = write(&wasm, None).unwrap_err();
        assert!(err.contains("host module `kv`"), "{err}");
    }

    #[test]
    fn worker_names() {
        assert_eq!(worker_name("My_App"), "my-app");
        assert_eq!(worker_name("__"), "velt-worker");
        assert_eq!(worker_name(&"a".repeat(80)).len(), 63);
    }
}

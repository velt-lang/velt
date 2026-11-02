//! `velt build --host <package>`: a build for the JavaScript platform of a host package
//! (docs/internals/contracts/workers.md). The compiler knows no platform: the host package's
//! `package.vlt` (`vpm::host`) gives the target, the condition, the platform's entry and the
//! configuration files to scaffold. The build writes a directory holding the module
//! (`program.wasm`), the runtime glue (velt_web.mjs, as every browser build gets, and its
//! host-neutral server core velt_serve.mjs), `velt_modules.mjs` (the package host modules the
//! module imports, each copied to `hosts/<package>/…`), the host package's entry, and its
//! scaffolded files when there are none, so edited ones are kept.

use std::path::{Path, PathBuf};

use vpm::graph::GraphPackage;
use vpm::host::{HostConfig, BUILD_FILES, MARKER};
use vpm::host_modules::RUNTIME_PREFIX;
use vpm::PackageGraph;

use crate::driver::HostBuild;

/// The glue's host-neutral server core (see the file's header).
const CORE: &str = include_str!("../../../velt_rt_wasm/js/velt_serve.mjs");
const CORE_FILE: &str = "velt_serve.mjs";
/// The map of the package host modules the module imports, which the entry passes to the core.
const MODULES_FILE: &str = "velt_modules.mjs";
/// The module's name in the directory, without the `.wasm` the driver adds.
const MODULE_STEM: &str = "program";
/// Where the package host modules go, below the directory.
const HOSTS_DIR: &str = "hosts";
/// The placeholder in a scaffolded file that becomes the program's name.
const NAME_PLACEHOLDER: &str = "{{name}}";
/// The custom section listing the module's host imports (velt_vir `vir::host::MANIFEST_SECTION`).
const MANIFEST_SECTION: &str = velt_vir::vir::MANIFEST_SECTION;

/// The host package `name` among the build's packages, which sets the build's target (a
/// `--target` naming another one is an error), for the program named `program` (a file's stem, a
/// package's name).
pub fn resolve(
    name: &str,
    packages: Option<&PackageGraph>,
    target: Option<&str>,
    program: &str,
) -> Result<HostBuild, String> {
    let Some(graph) = packages else {
        return Err(format!(
            "`--host {name}` builds a package: run it in a package that depends on `{name}`"
        ));
    };
    let Some(package) = graph.packages().iter().find(|p| p.name == name) else {
        return Err(format!(
            "`--host {name}`: no package `{name}` among the build's dependencies (add it with \
             `velt add {name}`)"
        ));
    };
    let Some(config) = &package.host else {
        return Err(format!(
            "`--host {name}`: package `{name}` is not a host package (its package.vlt has no \
             `host`)"
        ));
    };
    if !super::wasm::is_browser(&config.target) {
        return Err(format!(
            "host package `{name}` builds for `{}`, which the JavaScript glue does not run \
             (it runs `wasm32-unknown-unknown`)",
            config.target
        ));
    }
    if let Some(t) = target.filter(|t| t.trim() != config.target) {
        return Err(format!(
            "`--host {name}` builds for `{}`: leave out `--target {t}`",
            config.target
        ));
    }
    Ok(HostBuild {
        program: program.to_string(),
        package: name.to_string(),
        root: package.root.clone(),
        config: config.clone(),
    })
}

/// The module of a `--host` build of the program `host.program`, inside its directory: `-o`,
/// else `<program>-<condition>` in `base` (a build's default directory); the driver adds `.wasm`.
/// The directory must be one a host build may write into ([`check_dir`]).
pub fn module_path(output: Option<&Path>, base: &Path, host: &HostBuild) -> Result<PathBuf, String> {
    let dir = output.map_or_else(
        || base.join(format!("{}-{}", host.program, host.config.condition)),
        Path::to_path_buf,
    );
    check_dir(&dir, &host.config)?;
    Ok(dir.join(MODULE_STEM))
}

/// A host build writes into a directory that doesn't exist, is empty, holds nothing but files
/// its host package scaffolds, or that an earlier host build wrote (its [`MARKER`]): never over
/// files of the user's.
fn check_dir(dir: &Path, config: &HostConfig) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    if dir.join(MARKER).is_file() {
        return Ok(());
    }
    let scaffolded = |name: &str| {
        config
            .scaffold
            .keys()
            .any(|path| path.split('/').next() == Some(name))
    };
    let foreign: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !scaffolded(name))
        .collect();
    match foreign.first() {
        None => Ok(()),
        Some(first) => Err(format!(
            "`{}` holds files a `--host` build did not write (`{first}`…): build into an empty \
             or new directory (`-o <dir>`)",
            dir.display()
        )),
    }
}

/// After `module` was built into its directory: the rest of the build beside it.
pub fn write(module: &Path, host: &HostBuild, packages: Option<&PackageGraph>) -> Result<(), String> {
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
        copy(&package.root.join(file.trim_start_matches("./")), &dir.join(&rel))
            .map_err(|e| format!("cannot copy host module `{name}`: {e}"))?;
        modules.push((name, rel));
    }
    write_file(&dir.join(CORE_FILE), CORE)?;
    write_file(&dir.join(MODULES_FILE), &modules_map(&modules))?;
    let config = &host.config;
    copy(
        &host.root.join(config.entry.trim_start_matches("./")),
        &dir.join(config.entry_name()),
    )
    .map_err(|e| format!("cannot copy the entry of host package `{}`: {e}", host.package))?;
    let name = program_name(&host.program);
    for (path, file) in &config.scaffold {
        let to = dir.join(path);
        if to.exists() {
            continue;
        }
        let from = host.root.join(file.trim_start_matches("./"));
        let text = std::fs::read_to_string(&from)
            .map_err(|e| format!("cannot read `{}`: {e}", from.display()))?;
        create_parent(&to)?;
        write_file(&to, &text.replace(NAME_PLACEHOLDER, &name))?;
    }
    write_file(
        &dir.join(MARKER),
        &format!(
            "Written by `velt build --host {}`: later builds write their files here again \
             ({}, the entry), and keep the configuration files.\n",
            host.package,
            BUILD_FILES.join(", ")
        ),
    )
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

/// `velt_modules.mjs`: the host modules (`(name, path)`) by import module. Computed keys, so a
/// module named `__proto__` is a key, not the object's prototype.
fn modules_map(modules: &[(String, String)]) -> String {
    let lit = |s: &str| serde_json::to_string(s).expect("ICE: a string serializes");
    let mut out = String::from(
        "// The host modules the program imports, by import module: written by `velt build \
         --host` (each build writes it again).\n",
    );
    for (i, (_, path)) in modules.iter().enumerate() {
        out.push_str(&format!("import m{i} from {};\n", lit(&format!("./{path}"))));
    }
    let fields: Vec<String> = modules
        .iter()
        .enumerate()
        .map(|(i, (name, _))| format!("[{}]: m{i}", lit(name)))
        .collect();
    let map = match fields.is_empty() {
        true => "{}".to_string(),
        false => format!("{{ {} }}", fields.join(", ")),
    };
    out.push_str(&format!("export const modules = {map};\n"));
    out
}

/// A name from `stem` for configuration files: lowercase letters, digits and `-`, at most 63
/// characters (what platforms commonly accept for a service's name).
fn program_name(stem: &str) -> String {
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
        "velt-app".into()
    } else {
        name.into()
    }
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

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    create_parent(to)?;
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("cannot copy `{}`: {e}", from.display()))
}

fn create_parent(path: &Path) -> Result<(), String> {
    match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create `{}`: {e}", parent.display())),
        None => Ok(()),
    }
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;

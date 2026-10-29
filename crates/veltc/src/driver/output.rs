//! Where `build` puts its outputs: the executable and its objects (one per codegen unit).

use std::path::{Path, PathBuf};

use crate::cli::Emit;

/// Where `build` puts its outputs (relative paths are relative to the current directory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPaths {
    /// Linked executable (empty for `--emit obj`).
    pub executable: PathBuf,
    /// Object file.
    pub object: PathBuf,
}

impl OutputPaths {
    /// Object file of codegen unit `i`: [`Self::object`] for the first, `<stem>.cgu<i>.<ext>`
    /// beside it for the others.
    pub fn unit_object(&self, i: usize) -> PathBuf {
        if i == 0 {
            return self.object.clone();
        }
        let stem = self
            .object
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        match self.object.extension() {
            Some(ext) => self
                .object
                .with_file_name(format!("{stem}.cgu{i}.{}", ext.to_string_lossy())),
            None => self.object.with_file_name(format!("{stem}.cgu{i}")),
        }
    }

    /// Where the build links the std cache's objects from (`std_cache`): copies (hard links where
    /// the file system has them) named like the program's objects, in `<object stem>.std/`. The
    /// cache's files have no extension, which MSVC's linker reads as `<name>.obj`; and a copy is
    /// the object the build linked, whatever the cache's garbage collection does later.
    pub fn link_std_objects(&self, cached: &[PathBuf]) -> std::io::Result<Vec<PathBuf>> {
        let stem = self
            .object
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let dir = self.object.with_file_name(format!("{stem}.std"));
        let _ = std::fs::remove_dir_all(&dir);
        if cached.is_empty() {
            return Ok(vec![]);
        }
        std::fs::create_dir_all(&dir)?;
        let ext = self
            .object
            .extension()
            .map_or("o".into(), |e| e.to_string_lossy().into_owned());
        let mut out = Vec::with_capacity(cached.len());
        for (i, from) in cached.iter().enumerate() {
            let to = dir.join(format!("{i}.{ext}"));
            if std::fs::hard_link(from, &to).is_err() {
                std::fs::copy(from, &to)?;
            }
            out.push(to);
        }
        Ok(out)
    }

    /// Removes the objects of codegen units `count` and above, left by an earlier build with more
    /// units (so a directory listing or a manual link doesn't pick up stale code).
    pub fn remove_unit_objects_from(&self, count: usize) {
        for i in count.max(1).. {
            let path = self.unit_object(i);
            if std::fs::remove_file(&path).is_err() {
                break;
            }
        }
    }

    /// Output paths for `input` given `-o`, `--emit` and the target.
    pub fn new(input: &Path, output: Option<&Path>, emit: Emit, target: &str) -> OutputPaths {
        let dir = default_dir(input);
        if velt_codegen_llvm::is_wasm(target) {
            return Self::wasm(input, output, emit, &dir);
        }
        let windows = target.contains("windows");
        let obj_ext = if windows { "obj" } else { "o" };
        let stem = input
            .file_stem()
            .map_or_else(|| "main".into(), |s| s.to_string_lossy().into_owned());

        if emit == Emit::Obj {
            let object = match output {
                Some(o) => o.to_path_buf(),
                None => dir.join(format!("{stem}.{obj_ext}")),
            };
            return OutputPaths {
                executable: PathBuf::new(),
                object,
            };
        }

        let executable = match output {
            // `link.exe /OUT:foo` would produce an unrunnable `foo`; Windows executables need `.exe`.
            Some(o) if windows && o.extension().is_none() => o.with_extension("exe"),
            Some(o) => o.to_path_buf(),
            None => dir.join(if windows { format!("{stem}.exe") } else { stem }),
        };
        // Object next to the executable: `<exe name without .exe>.<obj_ext>`.
        let name = executable
            .file_name()
            .map_or_else(|| "main".into(), |s| s.to_string_lossy().into_owned());
        let base = if windows {
            name.strip_suffix(".exe").unwrap_or(&name)
        } else {
            &name
        };
        let object = executable.with_file_name(format!("{base}.{obj_ext}"));
        OutputPaths { executable, object }
    }

    /// WebAssembly outputs: `<stem>.wasm` (a module, not an executable) and `<stem>.o`.
    fn wasm(input: &Path, output: Option<&Path>, emit: Emit, default_dir: &Path) -> OutputPaths {
        let stem = input
            .file_stem()
            .map_or_else(|| "main".into(), |s| s.to_string_lossy().into_owned());
        if emit == Emit::Obj {
            let object =
                output.map_or_else(|| default_dir.join(format!("{stem}.o")), Path::to_path_buf);
            return OutputPaths {
                executable: PathBuf::new(),
                object,
            };
        }
        let executable = match output {
            Some(o) if o.extension().is_none() => o.with_extension("wasm"),
            Some(o) => o.to_path_buf(),
            None => default_dir.join(format!("{stem}.wasm")),
        };
        let object = executable.with_extension("o");
        OutputPaths { executable, object }
    }
}

/// Where a build without `-o` puts its outputs: `target/velt` of the package `input` belongs to,
/// wherever `velt` runs, so the members of a workspace built from its root keep their programs and
/// build stamps apart (docs/internals/design/workspaces.md). Relative (to the current directory)
/// when that is the package root, or `input` is in no package.
fn default_dir(input: &Path) -> PathBuf {
    let relative = Path::new("target").join("velt");
    let Some(root) = input.parent().and_then(vpm::manifest::find_package_root) else {
        return relative;
    };
    let cwd = std::env::current_dir().map(|d| vpm::relpath::absolute(&d));
    if cwd.is_ok_and(|cwd| vpm::relpath::normalize(&cwd) == vpm::relpath::normalize(&root)) {
        return relative;
    }
    root.join(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: &str = "x86_64-pc-windows-msvc";
    const LINUX: &str = "x86_64-unknown-linux-gnu";

    fn paths(input: &str, out: Option<&str>, emit: Emit, target: &str) -> (PathBuf, PathBuf) {
        let p = OutputPaths::new(Path::new(input), out.map(Path::new), emit, target);
        (p.executable, p.object)
    }

    #[test]
    fn codegen_unit_objects_sit_beside_the_first() {
        let p = OutputPaths::new(
            Path::new("app.vlt"),
            Some(Path::new("out/app")),
            Emit::Exe,
            LINUX,
        );
        assert_eq!(p.unit_object(0), PathBuf::from("out/app.o"));
        assert_eq!(p.unit_object(2), PathBuf::from("out/app.cgu2.o"));
        let p = OutputPaths::new(Path::new("app.vlt"), None, Emit::Exe, WIN);
        let dir = Path::new("target").join("velt");
        assert_eq!(p.unit_object(1), dir.join("app.cgu1.obj"));
    }

    #[test]
    fn fewer_codegen_units_remove_stale_objects() {
        let dir = tempfile::tempdir().unwrap();
        let p = OutputPaths::new(
            Path::new("app.vlt"),
            Some(&dir.path().join("app")),
            Emit::Exe,
            LINUX,
        );
        for i in 0..4 {
            std::fs::write(p.unit_object(i), b"obj").unwrap();
        }
        p.remove_unit_objects_from(2);
        let exists: Vec<bool> = (0..4).map(|i| p.unit_object(i).exists()).collect();
        assert_eq!(exists, [true, true, false, false]);
        p.remove_unit_objects_from(1);
        assert!(p.unit_object(0).exists() && !p.unit_object(1).exists());
    }

    #[test]
    fn default_output_paths() {
        let dir = Path::new("target").join("velt");
        assert_eq!(
            paths("tests/hello.vlt", None, Emit::Exe, WIN),
            (dir.join("hello.exe"), dir.join("hello.obj"))
        );
        assert_eq!(
            paths("hello.vlt", None, Emit::Exe, LINUX),
            (dir.join("hello"), dir.join("hello.o"))
        );
        assert_eq!(
            paths("a.b.vlt", None, Emit::Exe, LINUX),
            (dir.join("a.b"), dir.join("a.b.o"))
        );
        assert_eq!(
            paths("hello.vlt", None, Emit::Obj, LINUX).1,
            dir.join("hello.o")
        );
    }

    #[test]
    fn explicit_output_paths() {
        assert_eq!(
            paths("h.vlt", Some("out/app"), Emit::Exe, WIN),
            (PathBuf::from("out/app.exe"), PathBuf::from("out/app.obj"))
        );
        assert_eq!(
            paths("h.vlt", Some("out/app.exe"), Emit::Exe, WIN).1,
            PathBuf::from("out/app.obj")
        );
        assert_eq!(
            paths("h.vlt", Some("out/app"), Emit::Exe, LINUX),
            (PathBuf::from("out/app"), PathBuf::from("out/app.o"))
        );
        assert_eq!(
            paths("h.vlt", Some("x.o"), Emit::Obj, LINUX).1,
            PathBuf::from("x.o")
        );
    }

    #[test]
    fn a_package_s_outputs_go_to_its_own_target() {
        let tmp = tempfile::tempdir().unwrap();
        let member = vpm::relpath::absolute(tmp.path()).join("apps/server");
        std::fs::create_dir_all(member.join("src")).unwrap();
        std::fs::write(member.join("package.vlt"), "").unwrap();
        let p = OutputPaths::new(&member.join("src/main.vlt"), None, Emit::Exe, LINUX);
        assert_eq!(p.executable, member.join("target/velt/main"));
        assert_eq!(p.object, member.join("target/velt/main.o"));
    }

    #[test]
    fn wasm_output_paths() {
        let dir = Path::new("target").join("velt");
        assert_eq!(
            paths("hi.vlt", None, Emit::Exe, "wasm32-wasip1"),
            (dir.join("hi.wasm"), dir.join("hi.o"))
        );
        assert_eq!(
            paths(
                "hi.vlt",
                Some("out/app"),
                Emit::Exe,
                "wasm32-unknown-unknown"
            ),
            (PathBuf::from("out/app.wasm"), PathBuf::from("out/app.o"))
        );
        assert_eq!(
            paths("hi.vlt", None, Emit::Obj, "wasm32-wasip1").1,
            dir.join("hi.o")
        );
    }
}

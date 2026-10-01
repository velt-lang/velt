//! `velt doc`: HTML API docs (velt_doc) for the current package, given files/directories, or
//! the standard library (`--std`).

use std::path::{Path, PathBuf};

use velt_doc::Input;

use super::project::Project;
use crate::cli::DocArgs;

/// Generate the docs and print where they are.
pub fn doc_command(args: &DocArgs) -> Result<(), String> {
    let (title, intro, inputs, default_out) = collect(args)?;
    if inputs.is_empty() {
        return Err("no .vlt files to document".into());
    }
    let out = args.output.clone().unwrap_or(default_out);
    let index = velt_doc::write_api_docs(&title, &intro, &inputs, &out)?;
    eprintln!(
        "velt doc: {} modules → {}",
        inputs.len(),
        vpm::relpath::absolute(&index).display()
    );
    Ok(())
}

type Collected = (String, String, Vec<Input>, PathBuf);

fn collect(args: &DocArgs) -> Result<Collected, String> {
    let default_out = PathBuf::from("target").join("doc");
    if args.std {
        let root =
            crate::loader::std_root().ok_or("cannot find the standard library; set VELT_STD")?;
        let intro = "`import { … } from \"velt:<name>\"`; `std/prelude/*` is available everywhere.";
        let inputs = velt_doc::inputs_from_dir(&root, "std")?;
        return Ok((
            "Velt standard library".into(),
            intro.into(),
            inputs,
            default_out,
        ));
    }
    if !args.paths.is_empty() {
        let mut inputs = vec![];
        for p in &args.paths {
            inputs.extend(inputs_for_path(p)?);
        }
        return Ok((
            "API documentation".into(),
            String::new(),
            inputs,
            default_out,
        ));
    }
    let root = Project::current_root()?;
    let manifest = vpm::Manifest::from_dir(&root)?;
    let name = manifest.package.name.clone();
    let inputs = velt_doc::inputs_from_dir(&root.join("src"), &name)?
        .into_iter()
        .map(|mut i| {
            // `import … from "pkg"` is src/lib.vlt.
            if i.module == format!("{name}/lib") {
                i.module = name.clone();
            }
            i
        })
        .collect();
    let intro = format!("Version {}.", manifest.package.version);
    Ok((name, intro, inputs, root.join("target").join("doc")))
}

fn inputs_for_path(path: &Path) -> Result<Vec<Input>, String> {
    if path.is_dir() {
        let prefix = path
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        return velt_doc::inputs_from_dir(path, &prefix);
    }
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let module = path
        .file_stem()
        .map_or("main".into(), |s| s.to_string_lossy().into_owned());
    Ok(vec![Input { module, source }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_given_files_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("one.vlt");
        std::fs::write(&file, "export function f() {}\n").unwrap();
        let dir = tmp.path().join("lib");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("two.vlt"), "export const X: i64 = 1;\n").unwrap();
        let out = tmp.path().join("doc");
        let args = DocArgs {
            paths: vec![file, dir],
            std: false,
            output: Some(out.clone()),
        };
        doc_command(&args).unwrap();
        assert!(out.join("one.html").is_file());
        assert!(out.join("lib.two.html").is_file());
        let empty = DocArgs {
            paths: vec![tmp.path().join("doc")],
            ..args
        };
        assert!(doc_command(&empty).unwrap_err().contains("no .vlt files"));
    }

    #[test]
    fn std_docs() {
        let tmp = tempfile::tempdir().unwrap();
        let args = DocArgs {
            std: true,
            output: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        doc_command(&args).unwrap();
        let fs = std::fs::read_to_string(tmp.path().join("std.fs.html")).unwrap();
        assert!(fs.contains("readFile"), "{fs}");
    }
}

//! A minimal [`ProgramLoader`] for protocol tests: relative imports only (`./x` → `x.vlt` next to
//! the importer) and relative JSX runtimes (`// @jsxImportSource ./ui` → `ui/jsx-runtime.vlt`),
//! sources from the overlay or the disk, no std or packages. The real loader lives in
//! `veltc` (which depends on this crate) and has its own LSP integration test there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Diagnostics, SourceMap};
use velt_sema::SourceModule;
use velt_syntax::ast;

use crate::{LoadedProgram, ProgramLoader};

pub struct TestLoader;

impl ProgramLoader for TestLoader {
    fn load(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Result<LoadedProgram, String> {
        let read = |p: &Path| {
            overlay
                .get(p)
                .cloned()
                .or_else(|| std::fs::read_to_string(p).ok())
        };
        let src = read(root).ok_or_else(|| format!("cannot read `{}`", root.display()))?;
        let mut paths = vec![root.to_path_buf()];
        let mut modules = vec![parse(sm, root, src, "main".into(), diags)];
        let mut next = 0;
        while next < modules.len() {
            let dir = paths[next].parent().unwrap_or(Path::new("")).to_path_buf();
            let mut specs = import_specs(&modules[next].ast);
            let runtime = modules[next]
                .ast
                .jsx_import_source
                .as_ref()
                .map(|source| format!("{source}/jsx-runtime"));
            if let Some(r) = &runtime {
                specs.push((r.clone(), modules[next].ast.span));
            }
            for (spec, span) in specs {
                let Some(rel) = spec.strip_prefix("./") else {
                    continue;
                };
                let file = dir.join(format!("{rel}.vlt"));
                let index = match paths.iter().position(|p| *p == file) {
                    Some(i) => i,
                    None => match read(&file) {
                        Some(src) => {
                            paths.push(file.clone());
                            modules.push(parse(sm, &file, src, rel.into(), diags));
                            modules.len() - 1
                        }
                        None => {
                            let msg = format!("cannot find module `{spec}`");
                            diags.push(Diagnostic::error(msg, span));
                            continue;
                        }
                    },
                };
                let canonical = modules[index].path.clone();
                if runtime.as_ref() == Some(&spec) {
                    modules[next].jsx_runtime = Some(canonical);
                } else {
                    modules[next].imports.push((spec, canonical));
                }
            }
            next += 1;
        }
        Ok(LoadedProgram { modules, root: 0 })
    }
}

fn parse(
    sm: &mut SourceMap,
    path: &Path,
    src: String,
    canonical: String,
    diags: &mut Diagnostics,
) -> SourceModule {
    let file = sm.add(path, src);
    let (ast, parse_diags) = velt_syntax::parse_file(file, &sm.get(file).src);
    diags.extend(parse_diags);
    SourceModule {
        is_std: canonical.starts_with("std/"),
        path: canonical,
        file,
        ast,
        imports: vec![],
        jsx_runtime: None,
    }
}

fn import_specs(module: &ast::Module) -> Vec<(String, velt_common::Span)> {
    module
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Import(i) => Some((i.from.clone(), i.from_span)),
            _ => None,
        })
        .collect()
}

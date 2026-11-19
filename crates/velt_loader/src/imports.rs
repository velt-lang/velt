//! A module's imports: each specifier resolved to a file or a std module, which is loaded and
//! queued the first time it is asked for.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use velt_common::{std_modules, Span};
use velt_syntax::ast;

use crate::locate::Origin;
use crate::paths::{file_key, is_std_path};
use crate::{locate, pick, resolve_spec, wanted, Loader, ModuleRef};

impl Loader<'_, '_> {
    /// Resolve every import of module `index`, loading new modules (queued for their own imports).
    /// A std module's re-exports are followed only for the names asked of it ([`wanted`]).
    pub(crate) fn resolve_imports(&mut self, index: usize, queue: &mut VecDeque<usize>) {
        let is_std = self.modules[index].is_std;
        let syntax = self.base.get(index).unwrap_or(&self.modules[index]);
        let items: Vec<(ast::Import, bool)> = (syntax.ast.items.iter())
            .filter_map(|item| match &item.kind {
                ast::ItemKind::Import(imp) if !imp.from.is_empty() => {
                    Some((imp.clone(), item.exported))
                }
                // `declare module "spec" { … }` augments that module: it is loaded as an import.
                ast::ItemKind::Augment(ast::Augment {
                    module: Some((spec, span)),
                    ..
                }) => Some((wanted::augmented(spec, *span), false)),
                _ => None,
            })
            .collect();
        let mut imports: Vec<(String, Span, Option<Vec<String>>, bool)> = vec![];
        for (imp, exported) in items {
            let skip = exported && is_std && !self.wanted.needs(index, &imp);
            // A known import whose module is loaded is not pure (`pure` answers so, after finding
            // its file on disk: a load from std's unit has every std module loaded).
            if skip && !self.known_loaded(index, &imp.from) && self.pure(index, &imp.from) {
                continue;
            }
            let runner = self.harness == Some(index)
                && imp.from.starts_with(velt_common::std_specifier!(test));
            let written = !velt_syntax::is_synthesized_import(&imp) && !runner;
            imports.push((
                imp.from.clone(),
                imp.from_span,
                wanted::names_taken(&imp),
                written,
            ));
        }
        let first = self.resolved.insert(index);
        for (spec, span, names, written) in imports {
            let known = (self.modules[index].imports.iter())
                .find(|(s, _)| *s == spec)
                .map(|(_, path)| path.clone());
            let target = match known {
                Some(path) => self.module_by_path(&path),
                None => {
                    let target = self.import(index, &spec, span, written, queue);
                    if let Some(t) = target {
                        let canonical = self.modules[t].path.clone();
                        self.modules[index].imports.push((spec, canonical));
                    }
                    target
                }
            };
            if let Some(t) = target {
                if self.wanted.request(t, names) && self.resolved.contains(&t) && t != index {
                    queue.push_back(t);
                }
            }
        }
        if first {
            self.jsx_runtime(index, queue);
            self.load_globals(index, queue);
        }
    }

    /// Does module `index` already know where `spec` leads, to a module loaded? Then the import
    /// can't be left unloaded ([`Loader::pure`]).
    fn known_loaded(&mut self, index: usize, spec: &str) -> bool {
        let known = (self.modules[index].imports.iter())
            .find(|(s, _)| s == spec)
            .map(|(_, path)| path.clone());
        known.is_some_and(|path| self.module_by_path(&path).is_some())
    }

    /// The first module whose path is `path`. A load from std's unit resolves every std
    /// module's known imports, so a scan of the modules per import cost it a quadratic number of
    /// path comparisons.
    fn module_by_path(&mut self, path: &str) -> Option<usize> {
        while self.by_path_len < self.modules.len() {
            let i = self.by_path_len;
            (self.by_path)
                .entry(self.modules[i].path.clone())
                .or_insert(i);
            self.by_path_len += 1;
        }
        self.by_path.get(path).copied()
    }

    /// Locate (and load if new) the module `spec` imported by module `index`; `written`: the
    /// import is the source's, so std's visibility rule applies ([`visibility`]).
    pub(crate) fn import(
        &mut self,
        index: usize,
        spec: &str,
        span: Span,
        written: bool,
        queue: &mut VecDeque<usize>,
    ) -> Option<usize> {
        let (importer, from) = self.origins[index].clone();
        let dir = importer.parent().unwrap_or(Path::new(""));
        let alias = match vpm::imports::is_specifier(spec) {
            true => match self.conditional(spec, &importer, &from, span) {
                Ok(file) => Some(file),
                Err((msg, notes)) => return self.error(msg, notes, span),
            },
            false => self.path_alias(&importer, spec),
        };
        let mut aliased = alias.is_some();
        // No npm package was found for the specifier: an error that follows says why the
        // workspace walk may have missed it.
        let mut npm_missed = false;
        let module = match alias {
            Some(file) => ModuleRef::Relative { file },
            None => {
                let classified = resolve_spec(spec, dir);
                match locate::npm_module(
                    &mut self.npm,
                    self.opts.packages,
                    &importer,
                    spec,
                    &classified,
                ) {
                    Some(Ok(file)) => {
                        aliased = true;
                        ModuleRef::Relative { file }
                    }
                    Some(Err((msg, notes))) => return self.error(msg, notes, span),
                    None => {
                        npm_missed = matches!(classified, Ok(ModuleRef::Package { .. }));
                        match classified {
                            Ok(m) => m,
                            Err(msg) => {
                                let notes = self.npm.workspace_note(&importer);
                                return self.error(msg, notes.into_iter().collect(), span);
                            }
                        }
                    }
                }
            }
        };
        let target = match locate::target(
            module,
            &importer,
            &from,
            self.opts.std_root.as_deref(),
            self.opts.packages,
        ) {
            Ok(t) => t,
            Err((msg, mut notes)) => {
                if npm_missed {
                    notes.extend(self.npm.workspace_note(&importer));
                }
                return self.error(msg, notes, span);
            }
        };
        let shown = pick::Shown {
            dir,
            relative: aliased || spec.starts_with('.'),
        };
        let file = self.pick_file(spec, span, &target, &shown)?;
        let key = file_key(&file);
        let harness = self.harness == Some(index);
        let target = self.std_test_origin(target, &file, &key, harness);
        if written {
            if let Err((msg, notes)) = self.visible(&from, &target, &key, spec, harness) {
                return self.error(msg, notes, span);
            }
        }
        if let Err(msg) = self.std_membership(&target.origin, &key, spec) {
            return self.error(msg, vec![], span);
        }
        if let Some(&existing) = self.by_file.get(&key) {
            return Some(existing);
        }
        let src = match self.read_module(&file) {
            Ok(s) => s,
            Err(e) => {
                return self.error(
                    format!("cannot read module `{spec}` (`{}`): {e}", file.display()),
                    vec![],
                    span,
                )
            }
        };
        let canonical = target
            .canonical
            .unwrap_or_else(|| target.origin.canonical(&file));
        // `std/…` names the standard library: a user module with such a path (a `./std/`
        // directory, a path alias into one) would collide with it.
        let reserved = !matches!(target.origin, Origin::Std(_)) && is_std_path(&canonical);
        let canonical = if self.extra_phase && (reserved || self.is_taken(&canonical)) {
            self.fallback_name(&canonical)
        } else {
            canonical
        };
        if reserved && !self.extra_phase {
            let fix = if aliased {
                "change the `paths` alias"
            } else {
                "rename its `std` directory"
            };
            let msg = format!(
                "module `{spec}` would have the module path `{canonical}`, which is reserved for the standard library ({fix})"
            );
            return self.error(msg, vec![], span);
        }
        if self.is_taken(&canonical) {
            let msg = format!("module `{spec}` has the same module path `{canonical}` as another module (rename the file)");
            return self.error(msg, vec![], span);
        }
        let new = self.add(&file, key, src, canonical, target.origin);
        queue.push_back(new);
        Some(new)
    }

    /// The path a `paths` alias maps `spec` to, resolved like a relative import's (bare
    /// specifiers only: relative and `velt/` imports are never aliased).
    fn path_alias(&self, importer: &Path, spec: &str) -> Option<PathBuf> {
        if spec.starts_with("./")
            || spec.starts_with("../")
            || spec.starts_with(std_modules::SPECIFIER_PREFIX)
        {
            return None;
        }
        let file = self.opts.packages?.path_alias(importer, spec)?;
        Some(vpm::relpath::normalize(&file))
    }
}

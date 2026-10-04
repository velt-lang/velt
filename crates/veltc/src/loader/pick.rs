//! Picking the file of an import among its candidates ([`locate::Target`]): the first group with
//! an existing file wins, names must match the disk in case ([`case`]), two files in one group
//! make the import ambiguous, and a file module that hides a folder module of another extension
//! gets a warning.

use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Severity, Span};

use super::case::{respell, Case};
use super::locate::Target;
use super::{file_key, shown_path, Loader};

/// How an import's messages name files: relative to the importer's directory `dir` for relative
/// and path alias imports, in full otherwise.
pub(super) struct Shown<'a> {
    pub dir: &'a Path,
    pub relative: bool,
}

impl Shown<'_> {
    fn show(&self, file: &Path) -> String {
        shown_path(file, self.dir, self.relative)
    }
}

impl Loader<'_, '_> {
    /// The file `spec` names among `target`'s candidates, or `None` after reporting why there is
    /// none.
    pub(super) fn pick_file(
        &mut self,
        spec: &str,
        span: Span,
        target: &Target,
        shown: &Shown,
    ) -> Option<PathBuf> {
        let found = target.candidates.iter().enumerate().find_map(|(i, group)| {
            let existing: Vec<&PathBuf> = group
                .iter()
                .filter(|f| self.exists_exactly(f, &target.base))
                .collect();
            (!existing.is_empty()).then_some((i, existing))
        });
        match found {
            Some((group, files)) if files.len() == 1 => {
                let file = files[0].clone();
                self.warn_hidden_folder(spec, span, target, group, &file, shown);
                Some(file)
            }
            Some((_, files)) => {
                let mut names: Vec<String> = files
                    .iter()
                    .map(|f| format!("`{}`", shown.show(f)))
                    .collect();
                let last = names.pop().unwrap_or_default();
                let msg = format!(
                    "module `{spec}` is ambiguous: it could be {} or {last}",
                    names.join(", ")
                );
                let note = "rename or remove all but one of them".to_string();
                self.error(msg, vec![note], span);
                None
            }
            None => {
                self.not_found(spec, span, target, shown);
                None
            }
        }
    }

    /// Whether `file` exists under exactly this name (the part below `base`, case included), on
    /// disk or in the overlay.
    fn exists_exactly(&self, file: &Path, base: &Path) -> bool {
        if file.is_file() {
            return !matches!(self.dir_names.case_of(file, base), Case::Differs(_));
        }
        // A file only in the overlay: its key is the path as the editor spells it.
        !self.overlay.is_empty() && self.overlay.contains_key(&file_key(file))
    }

    /// Report that no candidate of `target` exists: as a difference in case when one exists
    /// under another case (with the specifier spelled right), else listing the files tried.
    fn not_found(&mut self, spec: &str, span: Span, target: &Target, shown: &Shown) {
        let differing = target.candidates.iter().find_map(|group| {
            group
                .iter()
                .find_map(|f| match self.dir_names.case_of(f, &target.base) {
                    Case::Differs(real) => Some((f.clone(), real)),
                    _ => None,
                })
        });
        if let Some((candidate, real)) = differing {
            let msg = format!(
                "module `{spec}` names `{}`, but the file is `{}`: file names must match in case",
                shown.show(&candidate),
                shown.show(&real)
            );
            let fix = match respell(spec, &candidate, &real) {
                Some(fixed) => format!("import it as `{fixed}`"),
                None => format!(
                    "rename `{}` to `{}`",
                    shown.show(&real),
                    shown.show(&candidate)
                ),
            };
            let note = "file systems on Linux tell names apart by case, so Velt does on every OS";
            self.error(msg, vec![fix, note.to_string()], span);
            return;
        }
        let notes = target
            .candidates
            .iter()
            .flatten()
            .map(|f| format!("tried `{}`", shown.show(f)))
            .collect();
        self.error(format!("cannot find module `{spec}`"), notes, span);
    }

    /// Warn when `file`, found in candidate group `group`, is a file module that hides a folder
    /// module (a later group) of another extension: `./ui` loads `ui.ts` although
    /// `ui/index.vlt` exists, as TypeScript does, which is easy to miss.
    fn warn_hidden_folder(
        &mut self,
        spec: &str,
        span: Span,
        target: &Target,
        group: usize,
        file: &Path,
        shown: &Shown,
    ) {
        let Some(folder) = target.candidates.get(group + 1) else {
            return;
        };
        // One `stat` for the usual case, no folder of that name.
        if !folder
            .first()
            .and_then(|f| f.parent())
            .is_some_and(Path::is_dir)
        {
            return;
        }
        let hidden = folder
            .iter()
            .find(|f| f.extension() != file.extension() && self.exists_exactly(f, &target.base));
        let Some(hidden) = hidden else {
            return;
        };
        let msg = format!(
            "module `{spec}` is the file `{}`, which hides the folder module `{}`",
            shown.show(file),
            shown.show(hidden)
        );
        let note = format!(
            "a file wins over a folder of the same name, as in TypeScript: rename one of them, or import the folder as `{spec}/index`"
        );
        let mut d = Diagnostic::error(msg, span).with_note(note);
        d.severity = Severity::Warning;
        self.diags.push(d);
    }
}

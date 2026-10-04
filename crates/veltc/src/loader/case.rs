//! Case-exact file names, like TypeScript's `forceConsistentCasingInFileNames`: the part of a
//! module's path an import spells must match the names on disk exactly, so `"./Util"` does not
//! load `util.vlt` on Windows or macOS (whose file systems ignore case) when it would fail on
//! Linux. Names come from directory listings, read once per directory for the whole load.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// How a path's names compare with the files on disk.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Case {
    /// Every name below the base is on disk exactly as spelled.
    Exact,
    /// The file exists under names that differ only in case: the path as named on disk.
    Differs(PathBuf),
    /// Some name is not on disk in any case (or is not UTF-8).
    Unknown,
}

/// The directory listings read so far, by directory.
#[derive(Default)]
pub(super) struct DirNames(RefCell<HashMap<PathBuf, Vec<String>>>);

impl DirNames {
    /// How the components of `file` below `base` compare with the names on disk. A file that is
    /// not below `base` is [`Case::Unknown`].
    pub(super) fn case_of(&self, file: &Path, base: &Path) -> Case {
        let Ok(rel) = file.strip_prefix(base) else {
            return Case::Unknown;
        };
        let mut real = base.to_path_buf();
        let mut differs = false;
        for component in rel.components() {
            let Component::Normal(name) = component else {
                real.push(component);
                continue;
            };
            let Some(name) = name.to_str() else {
                return Case::Unknown;
            };
            match self.find(&real, name) {
                Some(found) => {
                    differs |= found != name;
                    real.push(found);
                }
                None => return Case::Unknown,
            }
        }
        if differs {
            Case::Differs(real)
        } else {
            Case::Exact
        }
    }

    /// The entry of `dir` named `name`, else the only one whose name differs from it in case.
    fn find(&self, dir: &Path, name: &str) -> Option<String> {
        let mut cache = self.0.borrow_mut();
        let names = cache.entry(dir.to_path_buf()).or_insert_with(|| list(dir));
        if names.iter().any(|n| n == name) {
            return Some(name.to_string());
        }
        let lower = name.to_lowercase();
        let mut matches = names.iter().filter(|n| n.to_lowercase() == lower);
        let first = matches.next()?;
        matches.next().is_none().then(|| first.clone())
    }
}

/// The names in `dir` (the empty path is the current directory).
fn list(dir: &Path) -> Vec<String> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

/// `spec` with the names that differ in case between `candidate` (the file it was resolved to)
/// and `real` (that file as named on disk) spelled as on disk, or `None` when a name that differs
/// is not one `spec` spells (a folder's `index` file, a directory a path alias maps to).
pub(super) fn respell(spec: &str, candidate: &Path, real: &Path) -> Option<String> {
    let names = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    };
    let (candidate, real) = (names(candidate), names(real));
    if candidate.len() != real.len() {
        return None;
    }
    let mut pairs: Vec<(&str, &str)> = candidate
        .iter()
        .zip(&real)
        .map(|(c, r)| (c.as_str(), r.as_str()))
        .collect();
    let mut segments: Vec<String> = spec.split('/').map(str::to_string).collect();
    // A folder module's `index` file is a name the specifier doesn't spell.
    let stem = |name: &str| {
        vpm::sources::strip_source_extension(name)
            .unwrap_or(name)
            .to_string()
    };
    let last_segment = segments.last().map(|s| stem(s).to_lowercase());
    if let Some(&(c, r)) = pairs.last() {
        if stem(c) == "index" && last_segment.as_deref() != Some("index") {
            if c != r {
                return None;
            }
            pairs.pop();
        }
    }
    let mut segment = segments.len();
    while let Some((c, r)) = pairs.pop() {
        let spelled = segment
            .checked_sub(1)
            .filter(|&i| !matches!(segments[i].as_str(), "" | "." | ".."));
        let Some(i) = spelled else {
            // The rest of the path comes from the importer's directory: nothing to respell.
            if c != r || pairs.iter().any(|(c, r)| c != r) {
                return None;
            }
            break;
        };
        segment = i;
        if c != r {
            segments[i] = respell_segment(&segments[i], c, r)?;
        }
    }
    Some(segments.join("/"))
}

/// Specifier segment `segment`, which names the file or directory `candidate` (`Util` for
/// `Util.vlt`, `Util.js` for `Util.ts`), spelled like `real`.
fn respell_segment(segment: &str, candidate: &str, real: &str) -> Option<String> {
    if segment == candidate {
        return Some(real.to_string());
    }
    let candidate_stem = vpm::sources::strip_source_extension(candidate)?;
    let real_stem = vpm::sources::strip_source_extension(real)?;
    let rest = segment.strip_prefix(candidate_stem)?;
    (rest.is_empty() || rest.starts_with('.')).then(|| format!("{real_stem}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_compare_with_the_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        std::fs::create_dir_all(base.join("Lib")).unwrap();
        std::fs::write(base.join("Lib/util.vlt"), "").unwrap();
        let names = DirNames::default();
        assert_eq!(names.case_of(&base.join("Lib/util.vlt"), base), Case::Exact);
        assert_eq!(
            names.case_of(&base.join("lib/Util.vlt"), base),
            Case::Differs(base.join("Lib").join("util.vlt"))
        );
        assert_eq!(names.case_of(&base.join("Lib/x.vlt"), base), Case::Unknown);
        // Only the part below the base is compared.
        let sub = base.join("Lib");
        assert_eq!(names.case_of(&sub.join("util.vlt"), &sub), Case::Exact);
    }

    #[test]
    fn specifiers_are_respelled_as_on_disk() {
        let p = |s: &str| PathBuf::from(s);
        let cases = [
            ("./Util", "app/Util.vlt", "app/util.vlt", Some("./util")),
            ("./Util.js", "app/Util.ts", "app/util.ts", Some("./util.js")),
            (
                "./Util.tsx",
                "app/Util.tsx",
                "app/util.tsx",
                Some("./util.tsx"),
            ),
            ("../Lib/x", "Lib/x.vlt", "lib/x.vlt", Some("../lib/x")),
            ("./Ui", "app/Ui/index.ts", "app/ui/index.ts", Some("./ui")),
            (
                "./ui/Index",
                "app/ui/Index.ts",
                "app/ui/index.ts",
                Some("./ui/index"),
            ),
            // The index file itself is misspelled on disk: no specifier can fix that.
            ("./ui", "app/ui/index.ts", "app/ui/Index.ts", None),
            // A path alias mapping to a misspelled directory.
            ("@app/x", "SRC/x.vlt", "src/x.vlt", None),
            ("@app/X", "src/X.vlt", "src/x.vlt", Some("@app/x")),
            (
                "pkg/Sub",
                "pkg/src/Sub.ts",
                "pkg/src/sub.ts",
                Some("pkg/sub"),
            ),
        ];
        for (spec, candidate, real, fixed) in cases {
            assert_eq!(
                respell(spec, &p(candidate), &p(real)).as_deref(),
                fixed,
                "{spec}"
            );
        }
    }
}

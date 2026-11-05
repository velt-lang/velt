//! Which profile a source file has (docs/internals/design/lint-profiles.md §1): the rules that
//! apply to it and the fixes it may be given.

use std::path::{Path, PathBuf};

/// What a source file's code is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Shared with TypeScript (a `tsCompat` folder): it must compile with `tsc` and behave the
    /// same, so only fixes that stay in the common subset are offered.
    Portable,
    /// The package's own Velt code: every fix is offered.
    Native,
    /// Another package's code, the standard library's included: never reported, never fixed.
    Dependency,
}

impl Profile {
    /// Whether a fix that is (`portable`) or is not valid TypeScript may edit a file of this
    /// profile.
    pub fn allows(self, portable: bool) -> bool {
        match self {
            Profile::Native => true,
            Profile::Portable => portable,
            Profile::Dependency => false,
        }
    }
}

/// The profiles of one package's files: its root and its `tsCompat` folders, canonical.
#[derive(Clone, Debug, Default)]
pub struct Profiles {
    /// The package root; `None` for a file outside any package, whose loaded user files are all
    /// native.
    root: Option<PathBuf>,
    /// The `tsCompat` folders.
    portable: Vec<PathBuf>,
}

impl Profiles {
    /// The profiles of the package at `root` with the `tsCompat` folders `portable` (both in the
    /// form the files will be named in, canonical say).
    pub fn new(root: Option<PathBuf>, portable: Vec<PathBuf>) -> Self {
        Profiles { root, portable }
    }

    /// The profiles of the package around `dir` (none: every user file is native), from its
    /// manifest, canonicalized.
    pub fn around(dir: &Path) -> Self {
        let Some(root) = vpm::manifest::find_package_root(dir) else {
            return Profiles::default();
        };
        let canonical = |p: &Path| velt_tscompat::canonical(p);
        let portable = vpm::Manifest::from_dir(&root)
            .map(|m| {
                m.ts_compat_dirs(&root)
                    .iter()
                    .map(|d| canonical(d))
                    .collect()
            })
            .unwrap_or_default();
        Profiles::new(Some(canonical(&root)), portable)
    }

    /// The profile of the user file `path` (in the form the folders are in). A file of a
    /// dependency or the standard library is [`Profile::Dependency`] whatever its path: the
    /// caller checks that first.
    pub fn of(&self, path: &Path) -> Profile {
        if self
            .portable
            .iter()
            .any(|f| vpm::sources::in_folder(f, path))
        {
            return Profile::Portable;
        }
        match &self.root {
            Some(root) if !vpm::sources::in_folder(root, path) => Profile::Dependency,
            _ => Profile::Native,
        }
    }

    /// Whether any file is portable.
    pub fn has_portable(&self) -> bool {
        !self.portable.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_decide() {
        let tmp = std::env::temp_dir().join(format!("velt_lint_profiles_{}", std::process::id()));
        let shared = tmp.join("src/shared");
        std::fs::create_dir_all(&shared).unwrap();
        let p = Profiles::new(Some(tmp.clone()), vec![shared.clone()]);
        assert_eq!(p.of(&shared.join("App.tsx")), Profile::Portable);
        assert_eq!(p.of(&tmp.join("src/server.velt")), Profile::Native);
        assert_eq!(
            p.of(&tmp.join("node_modules/x/a.velt")),
            Profile::Dependency
        );
        assert_eq!(p.of(Path::new("/elsewhere/a.velt")), Profile::Dependency);
        // Outside a package every user file is native.
        assert_eq!(
            Profiles::default().of(&shared.join("a.velt")),
            Profile::Native
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn fixes_by_profile() {
        assert!(Profile::Native.allows(false) && Profile::Native.allows(true));
        assert!(Profile::Portable.allows(true) && !Profile::Portable.allows(false));
        assert!(!Profile::Dependency.allows(true));
    }
}

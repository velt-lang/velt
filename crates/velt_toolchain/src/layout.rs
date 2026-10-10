//! The toolchain root (`~/.velt`, `%LOCALAPPDATA%\velt` on Windows):
//!
//! ```text
//! <root>/bin/velt              the launcher, the only thing on PATH
//! <root>/toolchains/<version>/ a release's prefix (bin/, lib/, std/)
//! <root>/links/<name>          a file holding the path of a prefix built elsewhere
//! <root>/default               the toolchain used outside a package or without a pin
//! ```
//!
//! The root is found from the launcher's own path (`<root>/bin/velt`), so a root anywhere works
//! without configuration. Each prefix finds its own `lib/` and `std/` from its `bin/velt`, so the
//! launcher runs that file, never a link to it.

use std::fmt;
use std::path::{Path, PathBuf};

use semver::Version;

pub const BIN_DIR: &str = "bin";
pub const TOOLCHAINS_DIR: &str = "toolchains";
pub const LINKS_DIR: &str = "links";
pub const DEFAULT_FILE: &str = "default";

/// `<prefix>/bin/velt[.exe]`: the compiler of a toolchain prefix.
pub fn velt_exe(prefix: &Path) -> PathBuf {
    prefix
        .join(BIN_DIR)
        .join(format!("velt{}", std::env::consts::EXE_SUFFIX))
}

/// One toolchain: an installed release, or a prefix linked under a name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Toolchain {
    Version(Version),
    Linked(String),
}

impl Toolchain {
    /// A version (`0.1.3`) or a link name (`dev`).
    pub fn parse(text: &str) -> Result<Toolchain, String> {
        let text = text.trim();
        if let Ok(v) = Version::parse(text) {
            return Ok(Toolchain::Version(v));
        }
        check_link_name(text).map(|()| Toolchain::Linked(text.to_string()))
    }
}

impl fmt::Display for Toolchain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Toolchain::Version(v) => write!(f, "{v}"),
            Toolchain::Linked(name) => f.write_str(name),
        }
    }
}

/// A link name starts with a letter, so it is never read as a version, and is a single path
/// segment.
pub fn check_link_name(name: &str) -> Result<(), String> {
    let ok = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{name}` is neither a version (such as `0.1.0`) nor a toolchain name (lowercase \
             letters, digits, `-`, `_` and `.`, starting with a letter)"
        ))
    }
}

/// The directory holding the launcher and the toolchains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    dir: PathBuf,
}

impl Root {
    pub fn new(dir: impl Into<PathBuf>) -> Root {
        Root { dir: dir.into() }
    }

    /// The root of the launcher at `exe` (`<root>/bin/velt`).
    pub fn of_launcher(exe: &Path) -> Option<Root> {
        let bin = exe.parent()?;
        if bin.file_name()? != BIN_DIR {
            return None;
        }
        bin.parent().map(Root::new)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn toolchains_dir(&self) -> PathBuf {
        self.dir.join(TOOLCHAINS_DIR)
    }

    pub fn version_dir(&self, version: &Version) -> PathBuf {
        self.toolchains_dir().join(version.to_string())
    }

    /// The installed release versions, oldest first. Directories that are not a version
    /// (`.<v>.<pid>` staging) or have no `bin/velt` are skipped.
    pub fn versions(&self) -> Vec<Version> {
        let mut versions: Vec<Version> = std::fs::read_dir(self.toolchains_dir())
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter_map(|e| Version::parse(&e.file_name().to_string_lossy()).ok())
                    .filter(|v| velt_exe(&self.version_dir(v)).is_file())
                    .collect()
            })
            .unwrap_or_default();
        versions.sort();
        versions
    }

    /// The linked toolchains, by name, with the prefix each names.
    pub fn links(&self) -> Vec<(String, PathBuf)> {
        let mut links: Vec<(String, PathBuf)> = std::fs::read_dir(self.dir.join(LINKS_DIR))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        check_link_name(&name).ok()?;
                        let prefix = std::fs::read_to_string(e.path()).ok()?;
                        Some((name, PathBuf::from(prefix.trim_end_matches(['\n', '\r']))))
                    })
                    .collect()
            })
            .unwrap_or_default();
        links.sort();
        links
    }

    /// The prefix of `toolchain`, whether or not it is installed.
    pub fn prefix(&self, toolchain: &Toolchain) -> Result<PathBuf, String> {
        match toolchain {
            Toolchain::Version(v) => Ok(self.version_dir(v)),
            Toolchain::Linked(name) => self
                .links()
                .into_iter()
                .find(|(n, _)| n == name)
                .map(|(_, prefix)| prefix)
                .ok_or_else(|| {
                    format!("no toolchain is linked as `{name}` (`velt toolchain link {name} <prefix>`)")
                }),
        }
    }

    /// Whether `toolchain`'s compiler is there.
    pub fn is_installed(&self, toolchain: &Toolchain) -> bool {
        self.prefix(toolchain)
            .is_ok_and(|prefix| velt_exe(&prefix).is_file())
    }

    /// `<root>/default`, when it names a toolchain.
    pub fn default(&self) -> Result<Option<Toolchain>, String> {
        let path = self.dir.join(DEFAULT_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) if text.trim().is_empty() => Ok(None),
            Ok(text) => Toolchain::parse(&text)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    pub fn set_default(&self, toolchain: &Toolchain) -> Result<(), String> {
        write_file(&self.dir.join(DEFAULT_FILE), &format!("{toolchain}\n"))
    }

    /// Link `name` to the toolchain prefix `prefix` (which must hold `bin/velt`).
    pub fn link(&self, name: &str, prefix: &Path) -> Result<(), String> {
        check_link_name(name)?;
        if !velt_exe(prefix).is_file() {
            return Err(format!(
                "{} is not a toolchain prefix: it has no {}",
                prefix.display(),
                velt_exe(Path::new("")).display()
            ));
        }
        let prefix = std::path::absolute(prefix)
            .map_err(|e| format!("cannot resolve {}: {e}", prefix.display()))?;
        write_file(
            &self.dir.join(LINKS_DIR).join(name),
            &format!("{}\n", prefix.display()),
        )
    }

    /// Remove an installed version or a link (a link's prefix stays).
    pub fn remove(&self, toolchain: &Toolchain) -> Result<(), String> {
        let path = match toolchain {
            Toolchain::Version(v) => self.version_dir(v),
            Toolchain::Linked(name) => self.dir.join(LINKS_DIR).join(name),
        };
        let result = match toolchain {
            Toolchain::Version(_) => std::fs::remove_dir_all(&path),
            Toolchain::Linked(_) => std::fs::remove_file(&path),
        };
        match result {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(format!("toolchain {toolchain} is not installed"))
            }
            Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
        }
    }
}

/// Write `text` to `path` through a temporary file, so a reader never sees half of it.
fn write_file(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.{}", std::process::id()));
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write {}: {e}", path.display())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    pub(crate) fn fake_prefix(prefix: &Path) {
        std::fs::create_dir_all(prefix.join(BIN_DIR)).unwrap();
        std::fs::write(velt_exe(prefix), b"").unwrap();
    }

    #[test]
    fn the_root_is_found_from_the_launcher() {
        let root = Root::of_launcher(Path::new("/h/.velt/bin/velt")).unwrap();
        assert_eq!(root.dir(), Path::new("/h/.velt"));
        assert_eq!(
            root.version_dir(&v("0.1.0")),
            Path::new("/h/.velt/toolchains/0.1.0")
        );
        assert_eq!(Root::of_launcher(Path::new("/h/.velt/velt")), None);
    }

    #[test]
    fn installed_versions_links_and_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Root::new(tmp.path());
        assert!(root.versions().is_empty() && root.links().is_empty());
        assert_eq!(root.default().unwrap(), None);
        for ver in ["0.2.0", "0.1.0", "0.10.0"] {
            fake_prefix(&root.version_dir(&v(ver)));
        }
        // Staging directories, other names and prefixes without a compiler are not versions.
        std::fs::create_dir_all(root.toolchains_dir().join(".0.3.0.42/bin")).unwrap();
        std::fs::create_dir_all(root.toolchains_dir().join("0.4.0")).unwrap();
        std::fs::create_dir_all(root.toolchains_dir().join("notes")).unwrap();
        assert_eq!(root.versions(), [v("0.1.0"), v("0.2.0"), v("0.10.0")]);

        let dev = tmp.path().join("checkout/prefix");
        assert!(root
            .link("dev", &dev)
            .unwrap_err()
            .contains("not a toolchain prefix"));
        fake_prefix(&dev);
        root.link("dev", &dev).unwrap();
        let dev_tc = Toolchain::parse("dev").unwrap();
        assert_eq!(root.prefix(&dev_tc).unwrap(), dev);
        assert!(root.is_installed(&dev_tc));
        assert!(root.link("0.1.0", &dev).is_err() && root.link("../x", &dev).is_err());

        root.set_default(&Toolchain::Version(v("0.2.0"))).unwrap();
        assert_eq!(
            root.default().unwrap(),
            Some(Toolchain::Version(v("0.2.0")))
        );
        root.set_default(&dev_tc).unwrap();
        assert_eq!(root.default().unwrap(), Some(dev_tc.clone()));

        root.remove(&dev_tc).unwrap();
        assert!(dev.join("bin").is_dir(), "a link's prefix stays");
        assert!(!root.is_installed(&dev_tc));
        root.remove(&Toolchain::Version(v("0.1.0"))).unwrap();
        assert_eq!(root.versions(), [v("0.2.0"), v("0.10.0")]);
        assert!(root
            .remove(&Toolchain::Version(v("0.1.0")))
            .unwrap_err()
            .contains("not installed"));
    }

    #[test]
    fn toolchain_names() {
        assert_eq!(
            Toolchain::parse("0.1.0").unwrap(),
            Toolchain::Version(v("0.1.0"))
        );
        assert_eq!(
            Toolchain::parse("dev\n").unwrap(),
            Toolchain::Linked("dev".into())
        );
        for bad in ["0.1", "Dev", "", "a/b", "..", "1x"] {
            assert!(Toolchain::parse(bad).is_err(), "{bad}");
        }
    }
}

//! The `velt` field of `package.vlt`: which toolchain versions build the package.
//!
//! A bare version is "the newest patch of that minor" (.NET's `latestPatch`): `"0.1"` is any
//! `0.1.x`, `"0.1.3"` is `0.1.3` or a later `0.1.x`, and `"1"` any `1.x`. A requirement with an
//! operator (`"=0.1.3"`, `">=0.1, <0.3"`, `"^1.2"`) means what it means for dependencies. Unlike
//! a dependency, `"1.2"` therefore does not accept `1.3`: a new minor may change the language.

use std::fmt;

use semver::{Version, VersionReq};

/// A parsed `velt` requirement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requirement {
    text: String,
    req: VersionReq,
}

impl Requirement {
    /// Parse a requirement; the message names the value, so callers prefix it with the field.
    pub fn parse(text: &str) -> Result<Requirement, String> {
        let trimmed = text.trim();
        let bare = trimmed.starts_with(|c: char| c.is_ascii_digit());
        let source = if bare {
            format!("~{trimmed}")
        } else {
            trimmed.to_string()
        };
        let req = VersionReq::parse(&source)
            .map_err(|e| format!("`{text}` is not a version requirement: {e}"))?;
        Ok(Requirement {
            text: text.to_string(),
            req,
        })
    }

    /// The requirement for exactly `version` (`=<version>`).
    pub fn exact(version: &Version) -> Requirement {
        Requirement::parse(&format!("={version}")).expect("ICE: `=<version>` is a requirement")
    }

    pub fn matches(&self, version: &Version) -> bool {
        self.req.matches(version)
    }

    /// The newest of `versions` the requirement accepts.
    pub fn best<'v>(&self, versions: impl IntoIterator<Item = &'v Version>) -> Option<&'v Version> {
        versions.into_iter().filter(|v| self.matches(v)).max()
    }

    /// The exact version the requirement names, when it can only match one (`=0.1.3`).
    pub fn exact_version(&self) -> Option<Version> {
        match self.req.comparators.as_slice() {
            [c] if c.op == semver::Op::Exact => Some(Version {
                major: c.major,
                minor: c.minor?,
                patch: c.patch?,
                pre: c.pre.clone(),
                build: semver::BuildMetadata::EMPTY,
            }),
            _ => None,
        }
    }

    /// The text as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// The `velt` field `velt new` writes for toolchain `version`: its `major.minor`.
pub fn pin_for(version: &Version) -> String {
    format!("{}.{}", version.major, version.minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn accepts(req: &str, version: &str) -> bool {
        Requirement::parse(req).unwrap().matches(&v(version))
    }

    #[test]
    fn a_bare_version_is_the_newest_patch_of_its_minor() {
        assert!(accepts("0.1", "0.1.0") && accepts("0.1", "0.1.9"));
        assert!(!accepts("0.1", "0.2.0") && !accepts("0.1", "0.0.9"));
        assert!(accepts("0.1.3", "0.1.3") && accepts("0.1.3", "0.1.4"));
        assert!(!accepts("0.1.3", "0.1.2") && !accepts("0.1.3", "0.2.0"));
        // Not `^`: a new minor may change the language, also after 1.0.
        assert!(accepts("1.2", "1.2.7") && !accepts("1.2", "1.3.0"));
        assert!(accepts("1", "1.9.0") && !accepts("1", "2.0.0"));
        assert!(accepts(" 0.1 ", "0.1.2"));
    }

    #[test]
    fn operators_mean_what_they_mean_for_dependencies() {
        assert!(accepts("=0.1.3", "0.1.3") && !accepts("=0.1.3", "0.1.4"));
        assert!(accepts(">=0.1, <0.3", "0.2.5") && !accepts(">=0.1, <0.3", "0.3.0"));
        assert!(accepts("^1.2", "1.3.0"));
        assert!(accepts("*", "0.4.0"));
    }

    #[test]
    fn pre_releases_match_only_when_named() {
        assert!(!accepts("0.2", "0.2.0-rc.1"));
        assert!(accepts("0.2.0-rc.1", "0.2.0-rc.1"));
        assert!(accepts("0.2.0-rc.1", "0.2.0"));
    }

    #[test]
    fn garbage_is_an_error_naming_the_value() {
        for bad in ["", "latest", "0.x.y", "v0.1", "0.1.2.3", "=="] {
            let err = Requirement::parse(bad).unwrap_err();
            assert!(err.contains(&format!("`{bad}`")), "{bad}: {err}");
        }
    }

    #[test]
    fn best_and_exact() {
        let versions = [v("0.1.0"), v("0.1.4"), v("0.2.0"), v("0.1.2")];
        let req = Requirement::parse("0.1").unwrap();
        assert_eq!(req.best(&versions), Some(&v("0.1.4")));
        assert_eq!(Requirement::parse("0.3").unwrap().best(&versions), None);
        assert_eq!(req.exact_version(), None);
        assert_eq!(
            Requirement::parse("=0.1.3").unwrap().exact_version(),
            Some(v("0.1.3"))
        );
        assert_eq!(Requirement::parse("=0.1").unwrap().exact_version(), None);
        assert!(Requirement::exact(&v("0.2.0-rc.1")).matches(&v("0.2.0-rc.1")));
        assert_eq!(pin_for(&v("0.1.7")), "0.1");
    }
}

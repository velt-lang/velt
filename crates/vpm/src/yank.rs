//! `velt yank`: withdraw a published version, or bring it back. A yanked version stays in the
//! registry and downloadable, so lockfiles that pin it keep installing, but resolution never
//! picks it for a new requirement ([`crate::resolve`]). Versions are still immutable: yanking
//! changes only the index entry's `yanked` flag.

use crate::locations::Locations;
use crate::registry::{read_index, write_index};

/// Yank (`yanked`) or unyank `name` `version` in `loc`'s registry (remote or local).
pub fn yank(loc: &Locations, name: &str, version: &str, yanked: bool) -> Result<(), String> {
    let version = check(name, version)?;
    match &loc.remote {
        Some(url) => crate::remote::yank(url, name, &version, yanked),
        None => yank_local(loc, name, &version, yanked),
    }
}

/// [`yank`] in exactly the local registry of `loc`: what a registry server does.
pub fn yank_local(loc: &Locations, name: &str, version: &str, yanked: bool) -> Result<(), String> {
    let version = check(name, version)?;
    let loc = Locations {
        remote: None,
        ..loc.clone()
    };
    let mut index =
        read_index(&loc, name)?.ok_or_else(|| format!("`{name}` is not in the registry"))?;
    let entry = index
        .versions
        .iter_mut()
        .find(|v| v.version == version)
        .ok_or_else(|| format!("`{name}` {version} is not published"))?;
    if entry.yanked != yanked {
        entry.yanked = yanked;
        write_index(&loc, name, &index)?;
    }
    Ok(())
}

/// The normalized version, or why `name`@`version` can't be yanked.
fn check(name: &str, version: &str) -> Result<String, String> {
    if !crate::manifest::is_valid_package_name(name) {
        return Err(format!("invalid package name `{name}`"));
    }
    semver::Version::parse(version)
        .map(|v| v.to_string())
        .map_err(|e| format!("`{version}` is not a version: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::publish;

    #[test]
    fn yanks_and_unyanks_in_a_local_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = Locations::under(&tmp.path().join("home"));
        let pkg = tmp.path().join("lib");
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("package.vlt"),
            r#"export const pkg: Package = { name: "lib", version: "1.0.0" };"#,
        )
        .unwrap();
        std::fs::write(pkg.join("src/lib.vlt"), "").unwrap();
        publish(&pkg, &loc).unwrap();
        let yanked = |loc: &Locations| read_index(loc, "lib").unwrap().unwrap().versions[0].yanked;

        yank(&loc, "lib", "1.0.0", true).unwrap();
        assert!(yanked(&loc));
        let text = std::fs::read_to_string(loc.registry.join("lib/index.toml")).unwrap();
        assert!(text.contains("yanked = true"), "{text}");
        yank(&loc, "lib", "1.0.0", false).unwrap();
        assert!(!yanked(&loc));
        let text = std::fs::read_to_string(loc.registry.join("lib/index.toml")).unwrap();
        assert!(!text.contains("yanked"), "{text}");

        assert!(yank(&loc, "lib", "2.0.0", true)
            .unwrap_err()
            .contains("not published"));
        assert!(yank(&loc, "nope", "1.0.0", true)
            .unwrap_err()
            .contains("not in the registry"));
        assert!(yank(&loc, "lib", "one", true)
            .unwrap_err()
            .contains("not a version"));
    }
}

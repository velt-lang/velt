//! Workspace member globs (`members` of `velt.workspace.vlt`, and the npm workspaces vpm reads):
//! `/`-separated, `*` any run of characters within one directory name, `**` any number of
//! directories, `!` in front to exclude, names matched exactly (also where the file system ignores
//! case). The launcher asks [`lists`] whether the package it runs in is a member, so a package
//! below a workspace that the globs don't list keeps its own toolchain pin; vpm walks directories
//! with [`segment_match`] and excludes with [`glob_match`].

/// Whether `patterns` list the directory `rel` (relative to the workspace root, `/`-separated,
/// `"."` for the root): an including glob matches it and no excluding one does.
pub fn lists(patterns: &[String], rel: &str) -> bool {
    let segs = split(rel);
    let matching = |p: &str| glob_match(&split(p), &segs);
    let included = patterns
        .iter()
        .filter(|p| !p.starts_with('!'))
        .any(|p| matching(p));
    included
        && !patterns
            .iter()
            .filter_map(|p| p.strip_prefix('!'))
            .any(matching)
}

/// The segments of a glob or a relative path, without `./`, `.` and empty ones.
pub fn split(pattern: &str) -> Vec<&str> {
    let p = pattern.strip_prefix("./").unwrap_or(pattern);
    p.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

/// Whether the glob `parts` matches the path `segs`.
pub fn glob_match(parts: &[&str], segs: &[&str]) -> bool {
    match parts.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|i| glob_match(rest, &segs[i..])),
        Some((p, rest)) => segs
            .split_first()
            .is_some_and(|(s, more)| segment_match(p, s) && glob_match(rest, more)),
    }
}

/// Whether directory name `name` matches one glob segment (`*` any run of characters).
pub fn segment_match(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((pre, post)) => {
            name.len() >= pre.len() + post.len()
                && name.starts_with(pre)
                && segment_match_rest(post, &name[pre.len()..])
        }
    }
}

fn segment_match_rest(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => name.ends_with(pattern),
        Some((pre, post)) => (0..=name.len())
            .filter(|&i| name.is_char_boundary(i))
            .any(|i| {
                name[i..].starts_with(pre) && segment_match_rest(post, &name[i + pre.len()..])
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn globs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lists_members_by_glob() {
        let g = globs(&["packages/*", "apps/**", "!apps/old", "."]);
        assert!(lists(&g, "packages/core"));
        assert!(lists(&g, "apps/web/admin"));
        assert!(lists(&g, "."));
        assert!(!lists(&g, "apps/old"));
        assert!(!lists(&g, "packages/core/nested"));
        assert!(!lists(&g, "tools/x"));
        assert!(!lists(&globs(&["Apps/*"]), "apps/a"));
        assert!(segment_match("a*c", "abbc"));
        assert!(!segment_match("a*c", "abd"));
    }
}

//! What users of a version are told from the index of releases (advisories, yanks), from the
//! copy of the newest index fetched ([`super`] "The index of releases").

use super::*;

/// The copy of the newest index fetched ([`fetch_index`]), verified then: what notices about a
/// version use without the network.
pub const INDEX_CACHE_FILE: &str = "index-cache.json";

/// The index [`fetch_index`] last fetched into `root`, if any.
pub fn cached_index(root: &Root) -> Option<Index> {
    let text = std::fs::read_to_string(root.dir().join(INDEX_CACHE_FILE)).ok()?;
    parse_index(&text).ok()
}

/// What users of `version` should know, from `index`: it is yanked, or has an advisory; with
/// the newest release of its minor that is neither, when there is one. The index's text goes
/// through [`display_text`].
pub fn notice_for(index: &Index, version: &Version) -> Option<String> {
    let entry = index.releases.iter().find(|r| &r.version == version)?;
    let yanked = entry.yanked.as_deref().map(display_text);
    let advisory = entry.advisory.as_deref().map(display_text);
    if yanked.is_none() && advisory.is_none() {
        return None;
    }
    let mut problem = format!("velt {version}");
    if entry.security {
        problem.push_str(" has a security problem");
    }
    match (yanked, advisory) {
        (Some(why), advisory) => {
            let and = if entry.security { " and" } else { "" };
            problem.push_str(&format!("{and} is yanked: {why}"));
            if let Some(text) = advisory {
                problem.push_str(&format!("; {text}"));
            }
        }
        (None, Some(text)) => problem.push_str(&format!(": {text}")),
        (None, None) => {}
    }
    let fixed = index
        .releases
        .iter()
        .filter(|r| {
            r.version.major == version.major
                && r.version.minor == version.minor
                && r.version > *version
                && r.version.pre.is_empty()
                && r.yanked.is_none()
                && r.advisory.is_none()
        })
        .map(|r| &r.version)
        .max();
    Some(match fixed {
        Some(v) => format!(
            "{problem}; velt {v} doesn't have it (`velt update` in a package pinned to it, or \
             `velt toolchain install {v}`)"
        ),
        None => problem,
    })
}

/// The longest [`display_text`] keeps, in characters.
pub const DISPLAY_TEXT_MAX: usize = 200;

/// Text from the index of releases (a yank reason, an advisory) made safe to print: signed, but
/// still not trusted with the terminal. Control characters (escape sequences) and bidirectional
/// overrides become `?`, line breaks and tabs a space, and it is cut at [`DISPLAY_TEXT_MAX`]
/// characters with `…`.
pub fn display_text(text: &str) -> String {
    let mut shown = String::new();
    for (i, c) in text.trim().chars().enumerate() {
        if i == DISPLAY_TEXT_MAX {
            shown.push('…');
            break;
        }
        shown.push(match c {
            '\n' | '\r' | '\t' => ' ',
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => '?',
            c if c.is_control() => '?',
            c => c,
        });
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_for_yanked_and_advised_versions() {
        let index = parse_index(
            r#"{"format": 1, "generated": 1, "releases": [
                {"version": "0.2.0", "advisory": "TLS certificates are not checked", "security": true},
                {"version": "0.2.1", "yanked": "miscompiles closures"},
                {"version": "0.2.2"},
                {"version": "0.2.3-rc.1"},
                {"version": "0.3.0", "advisory": "slow startup"}
            ]}"#,
        )
        .unwrap();
        let v = |s: &str| Version::parse(s).unwrap();
        let security = notice_for(&index, &v("0.2.0")).unwrap();
        assert!(
            security.starts_with("velt 0.2.0 has a security problem: TLS certificates")
                && security.contains("velt 0.2.2 doesn't have it"),
            "{security}"
        );
        let yanked = notice_for(&index, &v("0.2.1")).unwrap();
        assert!(
            yanked.starts_with("velt 0.2.1 is yanked: miscompiles closures"),
            "{yanked}"
        );
        assert_eq!(notice_for(&index, &v("0.2.2")), None);
        assert_eq!(
            notice_for(&index, &v("0.3.0")).unwrap(),
            "velt 0.3.0: slow startup"
        );
        assert_eq!(notice_for(&index, &v("9.9.9")), None);
        // The fix it suggests.
        assert!(
            security.ends_with(
                "(`velt update` in a package pinned to it, or `velt toolchain install 0.2.2`)"
            ),
            "{security}"
        );
        // Yanked for a security problem: both said.
        let index = parse_index(
            r#"{"format": 1, "generated": 1, "releases": [
                {"version": "0.4.0", "yanked": "use 0.4.1", "advisory": "a crafted URL reads files",
                 "security": true}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            notice_for(&index, &v("0.4.0")).unwrap(),
            "velt 0.4.0 has a security problem and is yanked: use 0.4.1; a crafted URL reads files"
        );
        for bad in [
            r#"{"version": "0.1.0", "advisory": 1}"#,
            r#"{"version": "0.1.0", "security": "yes"}"#,
        ] {
            let text = format!(r#"{{"format": 1, "generated": 1, "releases": [{bad}]}}"#);
            assert!(parse_index(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn text_from_the_index_is_safe_to_print() {
        assert_eq!(display_text("  plain text "), "plain text");
        assert_eq!(
            display_text("\u{1b}[31mred\u{1b}]0;title\u{7}\r\nvelt: fake\tline"),
            "?[31mred?]0;title?  velt: fake line"
        );
        assert_eq!(display_text("a\u{202e}b\u{2066}c\u{7f}"), "a?b?c?");
        let long = display_text(&"x".repeat(20_000));
        assert_eq!(long.chars().count(), DISPLAY_TEXT_MAX + 1);
        assert!(long.ends_with('…'));
        assert_eq!(
            display_text(&"é".repeat(DISPLAY_TEXT_MAX)),
            "é".repeat(DISPLAY_TEXT_MAX)
        );
        // Notices use it.
        let index = parse_index(
            r#"{"format": 1, "generated": 1, "releases": [
                {"version": "0.1.0", "advisory": "\u001b[2Jgone\nvelt: all good"}]}"#,
        )
        .unwrap();
        let notice = notice_for(&index, &Version::parse("0.1.0").unwrap()).unwrap();
        assert_eq!(notice, "velt 0.1.0: ?[2Jgone velt: all good");
    }
}

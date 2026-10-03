use std::collections::BTreeMap;

use super::*;

const HEAD: &str = "export const pkg: Package = ";

fn entry(version: &str, yanked: bool) -> IndexEntry {
    IndexEntry {
        version: version.into(),
        checksum: "sha256:00".into(),
        dependencies: BTreeMap::new(),
        native_abi: None,
        native: BTreeMap::new(),
        yanked,
    }
}

/// `sqlite`: 0.1.0, 0.2.0, 0.2.5, 0.3.0-beta.1, 0.4.0 (yanked).
fn sqlite() -> Index {
    Index {
        versions: vec![
            entry("0.1.0", false),
            entry("0.2.0", false),
            entry("0.2.5", false),
            entry("0.3.0-beta.1", false),
            entry("0.4.0", true),
        ],
    }
}

/// `src` with the cursor at `|` (removed).
fn at(src: &str) -> (String, u32) {
    let offset = src.find('|').expect("a `|` cursor");
    (src.replacen('|', "", 1), offset as u32)
}

fn no_lock(_: &str) -> Option<String> {
    None
}

fn lookup(name: &str) -> Option<Option<Index>> {
    match name {
        "sqlite" => Some(Some(sqlite())),
        "gone" => Some(None),
        _ => None, // not known (yet)
    }
}

#[test]
fn dependencies_are_found_with_their_ranges() {
    let src = format!(
        "{HEAD}{{ name: \"a\", version: \"1.0.0\", dependencies: {{ sqlite: \"^0.2\", \"my-util\": {{ path: \"../u\", version: \"1\" }}, http: {{ version: \"0.3\" }} }} }};"
    );
    let deps = dependencies(&src);
    let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["sqlite", "my-util", "http"]);
    let (req, range) = deps[0].version.clone().unwrap();
    assert_eq!(
        (req.as_str(), &src[range.start as usize..range.end as usize]),
        ("^0.2", "^0.2")
    );
    assert!(deps[1].has_path && deps[1].version.as_ref().unwrap().0 == "1");
    assert_eq!(
        &src[deps[1].name_range.start as usize..deps[1].name_range.end as usize],
        "\"my-util\""
    );
    assert_eq!(deps[2].version.as_ref().unwrap().0, "0.3");
    assert_eq!(
        top_level_string(
            &format!("{HEAD}{{ registry: \"http://r\", name: }}"),
            "registry"
        ),
        Some("http://r".into())
    );
}

#[test]
fn the_cursor_asks_for_versions_or_names() {
    let ask = |body: &str| {
        cursor(
            &at(&format!("{HEAD}{body}")).0,
            at(&format!("{HEAD}{body}")).1,
        )
        .map(|c| c.ask)
    };
    let versions = Some(Ask::Versions {
        name: "sqlite".into(),
    });
    assert_eq!(ask("{ dependencies: { sqlite: \"^0.|\" } }"), versions);
    assert_eq!(ask("{ dependencies: { sqlite: | } }"), versions);
    assert_eq!(
        ask("{ dependencies: { sqlite: { version: \"|\" } } }"),
        versions
    );
    assert_eq!(
        ask("{ dependencies: { json: \"1\", sq| } }"),
        Some(Ask::Names {
            query: "sq".into(),
            taken: vec!["json".into()]
        })
    );
    assert_eq!(ask("{ dependencies: { sqlite: { path: \"|\" } } }"), None);
    assert_eq!(ask("{ name: \"|\" }"), None);
}

#[test]
fn versions_newest_first_without_yanked() {
    let (src, offset) = at(&format!("{HEAD}{{ dependencies: {{ sqlite: \"|\" }} }}"));
    let c = cursor(&src, offset).unwrap();
    let items = version_completions(&c, &sqlite());
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        ["^0.2.5", "0.3.0-beta.1", "0.2.5", "0.2.0", "0.1.0"]
    );
    assert_eq!(items[0].text, "^0.2.5", "inside quotes only the contents");
    assert!(items.windows(2).all(|w| w[0].sort < w[1].sort));
    // Outside quotes the string is written.
    let (src, offset) = at(&format!("{HEAD}{{ dependencies: {{ sqlite: | }} }}"));
    let items = version_completions(&cursor(&src, offset).unwrap(), &sqlite());
    assert_eq!(items[0].text, "\"^0.2.5\"");
}

#[test]
fn names_write_the_whole_entry() {
    let hits = vec![
        Hit {
            name: "sqlite".into(),
            version: "0.2.5".into(),
        },
        Hit {
            name: "sql-kit".into(),
            version: "1.0.0".into(),
        },
        Hit {
            name: "json".into(),
            version: "2.0.0".into(),
        },
    ];
    let (src, offset) = at(&format!("{HEAD}{{ dependencies: {{ json: \"2\", sq| }} }}"));
    let items = name_completions(&cursor(&src, offset).unwrap(), &hits);
    let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(
        texts,
        ["sqlite: \"^0.2.5$1\"", "\"sql-kit\": \"^1.0.0$1\""],
        "json is taken"
    );
    assert!(items.iter().all(|i| i.snippet));
}

#[test]
fn findings() {
    let index = sqlite();
    assert_eq!(check("^0.2", Some(&index), None), None);
    assert_eq!(
        check("^0.1", Some(&index), None),
        Some(Finding::Newer {
            newest: "0.2.5".parse().unwrap()
        })
    );
    assert_eq!(
        check("^0.4", Some(&index), None),
        Some(Finding::NoMatch {
            newest: Some("0.2.5".parse().unwrap())
        }),
        "0.4.0 is yanked"
    );
    assert_eq!(check("1", None, None), Some(Finding::NotInRegistry));
    assert_eq!(check("not a req", Some(&index), None), None);
}

#[test]
fn diagnostics_and_fixes() {
    let src = format!(
        "{HEAD}{{ dependencies: {{ sqlite: \"^0.1\", gone: \"1\", other: \"1\", local: {{ path: \"../l\", version: \"9\" }} }} }};"
    );
    let diags = diagnostics(&src, "http://r", &mut lookup, &no_lock);
    let shown: Vec<(String, &str)> = diags
        .iter()
        .map(|d| {
            let s = d.labels[0].span;
            (d.message.clone(), &src[s.lo as usize..s.hi as usize])
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                "`sqlite` 0.2.5 is available; `^0.1` does not include it".to_string(),
                "^0.1"
            ),
            (
                "package `gone` is not in the registry `http://r`".to_string(),
                "gone"
            ),
        ]
    );
    assert_eq!(diags[0].severity, velt_common::Severity::Note);
    assert!(diags[1].is_error());

    let at_sqlite = src.find("^0.1").unwrap() as u32;
    let fixes = fixes(&src, at_sqlite, at_sqlite, &mut lookup, &no_lock);
    assert_eq!(fixes.len(), 1);
    assert_eq!(fixes[0].text, "^0.2.5");
    assert_eq!(
        &src[fixes[0].range.start as usize..fixes[0].range.end as usize],
        "^0.1"
    );
    assert!(
        super::fixes(&src, 0, 1, &mut lookup, &no_lock).is_empty(),
        "nowhere near a dependency"
    );
}

#[test]
fn hover_on_a_dependency() {
    let src = format!("{HEAD}{{ dependencies: {{ sqlite: \"^0.1\" }} }};");
    let dep = dependency_at(&src, src.find("sqlite").unwrap() as u32 + 2).unwrap();
    let text = hover(&dep, Some(&sqlite()), Some("0.1.0"));
    assert_eq!(
        text,
        "**sqlite**  \nnewest: 0.2.5 (`^0.1` does not include it)  \nlocked: 0.1.0 (`velt.lock.json`)"
    );
    assert_eq!(hover(&dep, None, None), "**sqlite**  \nnot in the registry");
}

#[test]
fn newer_means_newer_than_every_match() {
    // A requirement on a pre-release is not told about an older stable release.
    let index = Index {
        versions: vec![entry("1.5.0", false), entry("2.0.0-beta.2", false)],
    };
    assert_eq!(check("^2.0.0-beta.1", Some(&index), None), None);
}

#[test]
fn a_locked_yanked_version_still_matches() {
    // 0.4.0 is yanked, but velt.lock.json pins it: resolution keeps it, so no error.
    assert_eq!(check("=0.4.0", Some(&sqlite()), Some("0.4.0")), None);
    assert_eq!(
        check("=0.4.0", Some(&sqlite()), None),
        Some(Finding::NoMatch {
            newest: Some("0.2.5".parse().unwrap())
        })
    );
    let src = format!("{HEAD}{{ dependencies: {{ sqlite: \"=0.4.0\" }} }};");
    let locked = |name: &str| (name == "sqlite").then(|| "0.4.0".to_string());
    assert!(diagnostics(&src, "r", &mut lookup, &locked).is_empty());
}

#[test]
fn invalid_names_never_reach_the_registry() {
    let src = format!("{HEAD}{{ dependencies: {{ \"../x\": \"1\" }} }};");
    let mut asked = vec![];
    let mut record = |name: &str| {
        asked.push(name.to_string());
        Some(None)
    };
    assert!(diagnostics(&src, "r", &mut record, &no_lock).is_empty());
    assert!(asked.is_empty(), "{asked:?}");
}

#[test]
fn a_pre_release_is_not_written_by_name_completion() {
    let hits = vec![Hit {
        name: "next".into(),
        version: "1.0.0-rc.1".into(),
    }];
    let (src, offset) = at(&format!("{HEAD}{{ dependencies: {{ ne| }} }}"));
    let items = name_completions(&cursor(&src, offset).unwrap(), &hits);
    assert_eq!(items[0].text, "next: \"$1\"");
}

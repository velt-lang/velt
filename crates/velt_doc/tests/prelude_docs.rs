//! Every exported global of the prelude (`std/prelude/*.vlt`) and every public member of its
//! classes, interfaces and `extend` blocks has a doc comment: the editor and `velt doc` show
//! them, and most of them are what TypeScript's lib says (`scripts/gen-prelude-docs.js`).

use std::path::Path;

use velt_doc::extract::{extract, DocItem, Kind};

fn undocumented(file: &str, owner: Option<&str>, item: &DocItem, out: &mut Vec<String>) {
    if item.name.starts_with("__") {
        return;
    }
    let name = match owner {
        Some(o) => format!("{o}.{}", item.name),
        None => item.name.clone(),
    };
    // An `extend` block is not a declaration of its own; its members are.
    if item.kind != Kind::Extension && item.doc.trim().is_empty() {
        out.push(format!("{file}: {name}"));
    }
    for m in &item.members {
        undocumented(file, Some(&item.name), m, out);
    }
}

#[test]
fn every_prelude_export_and_member_has_a_doc_comment() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/prelude");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("std/prelude")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "vlt"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no prelude files in {}", dir.display());
    let mut missing = Vec::new();
    for path in files {
        let src = std::fs::read_to_string(&path).expect("read the prelude");
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let module = extract(&file, &src);
        for item in &module.items {
            undocumented(&file, None, item, &mut missing);
        }
    }
    assert!(
        missing.is_empty(),
        "{} prelude declarations have no doc comment (`/** … */`; see \
         scripts/gen-prelude-docs.js for TypeScript's text):\n{}",
        missing.len(),
        missing.join("\n")
    );
}

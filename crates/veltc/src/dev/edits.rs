//! What a save changed in the program's own code (docs/internals/design/dev-warm-front-end.md
//! step 3, "A. Change detection"): the gate of the body-edit fast path. Each top-level definition
//! of the program's files is kept as its *shape* (its source text with every function, method and
//! constructor body cut out) and the text of each of those bodies, by name. A save then either
//! changed nothing, changed the bodies of some functions and nothing else, or changed something
//! else (a signature, a type, an import, a definition added or removed), which only the cold
//! front end handles. Comparing source text is conservative: a comment or a blank line inside a
//! body counts as a body edit, and one between definitions as another change.

use std::path::{Path, PathBuf};

use std::collections::{HashMap, HashSet};
use velt_common::{FileId, Span};
use velt_sema::interface::Change;
use velt_syntax::ast;

/// The program's own files, as [`classify`] compares them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    files: HashMap<PathBuf, File>,
    /// Every other file of the build but std's (a path dependency's), by a hash of its text:
    /// any change to one is a change the body-edit fast path does not take.
    others: Vec<(PathBuf, u64)>,
}

/// One file: its definitions in order, and what lies outside them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct File {
    /// Its text, which a later snapshot of the same text reuses the parse of.
    src: String,
    defs: Vec<Def>,
    /// The text between and around the definitions (comments, blank lines).
    rest: String,
}

/// One top-level definition.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Def {
    /// The definition's name (`greet`, `User`), or its kind and position for one without.
    name: String,
    /// Its text with every body cut out.
    shape: String,
    /// Its bodies by name (`greet`, `User.greet`, `User.constructor`), with their text.
    bodies: Vec<(String, String)>,
}

/// What a save changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Nothing in the program's own files.
    None,
    /// Only these function bodies (`greet`, `User.greet`), in file order.
    Bodies(Vec<String>),
    /// Something else, the first such change said in words.
    Other(String),
}

impl std::fmt::Display for Edit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Edit::None => write!(f, "no change to the program's code"),
            Edit::Bodies(names) => write!(f, "the bodies of {}", names.join(", ")),
            Edit::Other(why) => write!(f, "{why}"),
        }
    }
}

impl Snapshot {
    /// The program's own files of a build: those under the root module's directory that are not
    /// std's (a path dependency's files outside it are a package's).
    pub fn of_build(sess: &crate::driver::Session, root: &Path) -> Snapshot {
        Snapshot::of_build_after(sess, root, None)
    }

    /// [`Snapshot::of_build`], each file whose text `prev` has as it is taken from `prev`
    /// without parsing it again.
    pub fn of_build_after(
        sess: &crate::driver::Session,
        root: &Path,
        prev: Option<&Snapshot>,
    ) -> Snapshot {
        let dir = own_dir(root);
        let std: HashSet<FileId> = sess.std_files.iter().copied().collect();
        let (own, others): (Vec<_>, Vec<_>) = (sess.sm.files())
            .filter(|(id, _)| !std.contains(id))
            .partition(|(_, f)| {
                let path = std::fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone());
                path.starts_with(&dir)
            });
        let mut snapshot = Snapshot::of_after(
            own.iter().map(|(_, f)| (f.path.as_path(), f.src.as_str())),
            prev,
        );
        snapshot.others = (others.iter())
            .map(|(_, f)| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                f.src.hash(&mut h);
                (f.path.clone(), h.finish())
            })
            .collect();
        snapshot.others.sort();
        snapshot
    }

    /// The definitions of the given files (path, source), parsed on their own.
    pub fn of<'a>(files: impl IntoIterator<Item = (&'a Path, &'a str)>) -> Snapshot {
        Snapshot::of_after(files, None)
    }

    /// [`Snapshot::of`], reusing what `prev` has of a file of the same path and text.
    fn of_after<'a>(
        files: impl IntoIterator<Item = (&'a Path, &'a str)>,
        prev: Option<&Snapshot>,
    ) -> Snapshot {
        let files = (files.into_iter())
            .map(|(path, src)| {
                let kept = prev
                    .and_then(|p| p.files.get(path))
                    .filter(|f| f.src == src);
                let file = kept.cloned().unwrap_or_else(|| File::of(src));
                (path.to_path_buf(), file)
            })
            .collect();
        Snapshot {
            files,
            others: vec![],
        }
    }
}

/// The directory of the root module, canonical: a bare file name (`velt dev main.velt`) is in the
/// working directory. (An empty path would canonicalize to nothing, and every file starts with it.)
fn own_dir(root: &Path) -> PathBuf {
    let dir = match root.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

impl File {
    fn of(src: &str) -> File {
        let (module, _) = velt_syntax::parse_file(FileId(0), src);
        let mut file = File {
            src: src.to_string(),
            ..File::default()
        };
        let mut at = 0usize;
        // In source order, without the function the parser makes of the top-level statements
        // (`velt_syntax::is_script_main`, after the declarations and spanning them): the
        // statements are text outside the definitions, any edit of which the cold front end
        // takes.
        let script = |item: &ast::Item| matches!(&item.kind, ast::ItemKind::Function(f) if velt_syntax::is_script_main(&f.sig.name));
        let mut items: Vec<(usize, &ast::Item)> = (module.items.iter().enumerate())
            .filter(|(_, item)| !script(item))
            .collect();
        items.sort_by_key(|(_, item)| item.span.lo);
        for (i, item) in items {
            let (lo, hi) = (item.span.lo as usize, item.span.hi as usize);
            if lo < at || hi > src.len() {
                // Definitions that overlap (a script's generated `main` spanning the file):
                // compare the whole text.
                file.rest = src.to_string();
                file.defs.clear();
                return file;
            }
            file.rest.push_str(&src[at..lo]);
            file.rest.push('\u{0}');
            at = hi;
            file.defs.push(Def::of(src, item, i));
        }
        file.rest.push_str(&src[at..]);
        file
    }
}

impl Def {
    fn of(src: &str, item: &ast::Item, index: usize) -> Def {
        let mut bodies: Vec<(String, Span)> = vec![];
        let name = match &item.kind {
            ast::ItemKind::Function(f) => {
                bodies.push((f.sig.name.name.clone(), f.body.span));
                f.sig.name.name.clone()
            }
            ast::ItemKind::Class(t) | ast::ItemKind::Struct(t) => {
                let ty = t.name.name.clone();
                if let Some(c) = &t.constructor {
                    bodies.push((format!("{ty}.constructor"), c.body.span));
                }
                for m in &t.methods {
                    bodies.push((velt_sema::member_name(&ty, m), m.decl.body.span));
                }
                ty
            }
            _ => format!("#{index}"),
        };
        // Bodies the parser recovered without a span, or out of the item: none of them.
        let (lo, hi) = (item.span.lo as usize, item.span.hi as usize);
        bodies.retain(|(_, s)| lo <= s.lo as usize && s.lo <= s.hi && s.hi as usize <= hi);
        bodies.sort_by_key(|(_, s)| s.lo);
        let mut shape = String::new();
        let mut at = lo;
        let mut texts = vec![];
        for (name, s) in bodies {
            let (blo, bhi) = (s.lo as usize, s.hi as usize);
            if blo < at {
                continue;
            }
            shape.push_str(&src[at..blo]);
            shape.push_str("{…}");
            texts.push((name, src[blo..bhi].to_string()));
            at = bhi;
        }
        shape.push_str(&src[at..hi]);
        Def {
            name,
            shape,
            bodies: texts,
        }
    }
}

/// What changed from `old` to `new`.
pub fn classify(old: &Snapshot, new: &Snapshot) -> Edit {
    match located(old, new) {
        Err(why) => Edit::Other(why),
        Ok(changed) if changed.is_empty() => Edit::None,
        Ok(changed) => Edit::Bodies(changed.into_iter().map(|(_, name)| name).collect()),
    }
}

/// The function bodies changed from `old` to `new`, each with its file, in file order; why not
/// when something else changed.
pub fn located(old: &Snapshot, new: &Snapshot) -> Result<Vec<(PathBuf, String)>, String> {
    if old.others != new.others {
        return Err("a file outside the program's directory changed".into());
    }
    let mut paths: Vec<&PathBuf> = old.files.keys().chain(new.files.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut changed = vec![];
    for path in paths {
        let (Some(a), Some(b)) = (old.files.get(path), new.files.get(path)) else {
            return Err(format!("{} was added or removed", path.display()));
        };
        if a.rest != b.rest || a.defs.len() != b.defs.len() {
            return Err(format!(
                "{}: a definition, comment or import outside the functions changed",
                path.display()
            ));
        }
        for (x, y) in a.defs.iter().zip(&b.defs) {
            if x.name != y.name || x.shape != y.shape || x.bodies.len() != y.bodies.len() {
                return Err(format!("{}: `{}` changed", path.display(), y.name));
            }
            for ((xn, xt), (yn, yt)) in x.bodies.iter().zip(&y.bodies) {
                if xn != yn {
                    return Err(format!("{}: `{}` changed", path.display(), y.name));
                }
                if xt != yt {
                    changed.push((path.clone(), yn.clone()));
                }
            }
        }
    }
    Ok(changed)
}

/// What the interface summaries (velt_sema `interface`, design step 3 "B") of the running and
/// the new version say of a body edit: whether everything but the edited functions' bodies kept
/// its interface, which is when the fast path would have been sound. Closures of an edited
/// function are its own: their interfaces may change. `None` for any other edit.
pub fn cutoff(edit: &Edit, changes: &[Change]) -> Option<String> {
    let Edit::Bodies(edited) = edit else {
        return None;
    };
    let names = |n: &str, e: &str| n == e || n.strip_suffix(e).is_some_and(|m| m.ends_with("::"));
    let own_closure = |name: &str| {
        name.find("::{closure#")
            .is_some_and(|at| edited.iter().any(|e| names(&name[..at], e)))
    };
    let left: Vec<String> = (changes.iter())
        .filter(|c| !matches!(c, Change::Fn(name) if own_closure(name)))
        .map(|c| c.to_string())
        .collect();
    const SHOWN: usize = 6;
    Some(match left.len() {
        0 => "the same interfaces: a body-edit fast path applies".to_string(),
        n if n <= SHOWN => format!("interfaces changed: {}", left.join(", ")),
        n => format!(
            "interfaces changed: {} and {} more",
            left[..SHOWN].join(", "),
            n - SHOWN
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(before: &str, after: &str) -> Edit {
        let p = Path::new("main.velt");
        classify(&Snapshot::of([(p, before)]), &Snapshot::of([(p, after)]))
    }

    const V1: &str = "import { helper } from \"./util\";

class User {
  name: string = \"a\";
  constructor(n: string) { this.name = n; }
  greet(): string { return `hi ${this.name}`; }
}

function twice(n: number): number {
  return n * 2;
}

function main() {
  console.log(new User(\"x\").greet(), twice(2), helper());
}
";

    #[test]
    fn nothing_changed() {
        assert_eq!(edit(V1, V1), Edit::None);
    }

    #[test]
    fn a_function_body_or_a_method_body() {
        let after = V1.replace("return n * 2;", "return n * 2 + 1;");
        assert_eq!(edit(V1, &after), Edit::Bodies(vec!["twice".into()]));
        let after = V1.replace("`hi ${this.name}`", "`hello ${this.name}`");
        assert_eq!(edit(V1, &after), Edit::Bodies(vec!["User.greet".into()]));
        let after = V1
            .replace("this.name = n;", "this.name = n + \"!\";")
            .replace("twice(2)", "twice(3)");
        let both = vec!["User.constructor".into(), "main".into()];
        assert_eq!(edit(V1, &after), Edit::Bodies(both));
    }

    #[test]
    fn a_body_edit_that_moves_later_definitions_is_still_one() {
        // Later definitions move down; their text, shape and bodies stay the same.
        let after = V1.replace("return n * 2;", "const m = n;\n  return m * 2;");
        assert_eq!(edit(V1, &after), Edit::Bodies(vec!["twice".into()]));
    }

    #[test]
    fn a_module_with_top_level_statements_still_has_body_edits() {
        // The parser puts the statements after the declarations; the edit is still a body's.
        let v = "const xs: number[] = [];\nfunction add(n: number) { xs.push(n); }\n\
                 for (const n of [1, 2]) add(n);\nfunction size(): number { return xs.length; }\n\
                 console.log(size());\n";
        let after = v.replace("return xs.length;", "return xs.length + 1;");
        assert_eq!(edit(v, &after), Edit::Bodies(vec!["size".into()]));
        let after = v.replace("[1, 2]", "[1, 3]");
        assert!(matches!(edit(v, &after), Edit::Other(_)));
    }

    #[test]
    fn everything_else_is_another_change() {
        let other = |after: String| matches!(edit(V1, &after), Edit::Other(_));
        assert!(other(V1.replace(
            "(n: number): number",
            "(n: number, k: number): number"
        )));
        assert!(other(
            V1.replace("name: string = \"a\";", "name: string = \"b\";")
        ));
        assert!(other(
            V1.replace("import { helper }", "import { helper, more }")
        ));
        assert!(other(format!("{V1}\nfunction added() {{}}\n")));
        assert!(other(V1.replace("function twice", "function thrice")));
        assert!(other(
            V1.replace("class User {", "// a comment\nclass User {")
        ));
    }

    #[test]
    fn a_file_added_or_removed_is_another_change() {
        let p = Path::new("main.velt");
        let q = Path::new("util.velt");
        let one = Snapshot::of([(p, V1)]);
        let two = Snapshot::of([(p, V1), (q, "export function helper() { return 1; }")]);
        assert!(matches!(classify(&one, &two), Edit::Other(_)));
    }

    #[test]
    fn a_body_edit_keeps_interfaces_unless_the_summaries_differ() {
        let edit = Edit::Bodies(vec!["twice".into(), "User.greet".into()]);
        let f = |n: &str| Change::Fn(n.to_string());
        let same = cutoff(
            &edit,
            &[f("twice::{closure#0}"), f("app::User.greet::{closure#2}")],
        );
        assert!(same.unwrap().starts_with("the same interfaces"));
        let changed = cutoff(&edit, &[f("twice"), f("thrice::{closure#0}")]).unwrap();
        assert_eq!(changed, "interfaces changed: twice, thrice::{closure#0}");
        let fact = cutoff(&edit, &[Change::Fact("declarations")]).unwrap();
        assert_eq!(fact, "interfaces changed: the program's declarations");
        let many: Vec<Change> = (0..8).map(|i| f(&format!("g{i}"))).collect();
        assert!(cutoff(&edit, &many).unwrap().ends_with("and 2 more"));
        assert_eq!(cutoff(&Edit::None, &[]), None);
        assert_eq!(cutoff(&Edit::Other("x".into()), &[]), None);
    }

    #[test]
    fn a_bare_root_file_name_is_in_the_working_directory() {
        let cwd = std::fs::canonicalize(".").expect("cwd");
        assert_eq!(own_dir(Path::new("main.velt")), cwd);
        assert_eq!(own_dir(Path::new("./main.velt")), cwd);
        assert!(!own_dir(Path::new("main.velt")).as_os_str().is_empty());
    }
}

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
use velt_syntax::ast;

/// The program's own files, as [`classify`] compares them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    files: HashMap<PathBuf, File>,
}

/// One file: its definitions in order, and what lies outside them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct File {
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
        let dir = root.parent().unwrap_or(Path::new(""));
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let std: HashSet<FileId> = sess.std_files.iter().copied().collect();
        let own = (sess.sm.files())
            .filter(|(id, f)| {
                let path = std::fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone());
                !std.contains(id) && path.starts_with(&dir)
            })
            .map(|(_, f)| (f.path.as_path(), f.src.as_str()));
        Snapshot::of(own)
    }

    /// The definitions of the given files (path, source), parsed on their own.
    pub fn of<'a>(files: impl IntoIterator<Item = (&'a Path, &'a str)>) -> Snapshot {
        let files = (files.into_iter())
            .map(|(path, src)| (path.to_path_buf(), File::of(src)))
            .collect();
        Snapshot { files }
    }
}

impl File {
    fn of(src: &str) -> File {
        let (module, _) = velt_syntax::parse_file(FileId(0), src);
        let mut file = File::default();
        let mut at = 0usize;
        for (i, item) in module.items.iter().enumerate() {
            let (lo, hi) = (item.span.lo as usize, item.span.hi as usize);
            if lo < at || hi > src.len() {
                // A definition the parser placed out of order (a script's generated `main`
                // spanning the file): compare the whole text.
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
                    bodies.push((format!("{ty}.{}", m.decl.sig.name.name), m.decl.body.span));
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
    let mut paths: Vec<&PathBuf> = old.files.keys().chain(new.files.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut changed = vec![];
    for path in paths {
        let (Some(a), Some(b)) = (old.files.get(path), new.files.get(path)) else {
            return Edit::Other(format!("{} was added or removed", path.display()));
        };
        if a.rest != b.rest || a.defs.len() != b.defs.len() {
            return Edit::Other(format!(
                "{}: a definition, comment or import outside the functions changed",
                path.display()
            ));
        }
        for (x, y) in a.defs.iter().zip(&b.defs) {
            if x.name != y.name || x.shape != y.shape || x.bodies.len() != y.bodies.len() {
                return Edit::Other(format!("{}: `{}` changed", path.display(), y.name));
            }
            for ((xn, xt), (yn, yt)) in x.bodies.iter().zip(&y.bodies) {
                if xn != yn {
                    return Edit::Other(format!("{}: `{}` changed", path.display(), y.name));
                }
                if xt != yt {
                    changed.push(yn.clone());
                }
            }
        }
    }
    match changed.is_empty() {
        true => Edit::None,
        false => Edit::Bodies(changed),
    }
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
}

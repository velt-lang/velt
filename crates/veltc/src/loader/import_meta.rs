//! `import.meta` (`ast::IMPORT_META`): the parser adds a module constant with placeholder fields
//! to a module that mentions it, and the loader, which knows the module's file, fills them in as
//! Node does: `url` is the file's `file:` URL, `filename` its absolute path and `dirname` its
//! directory, `main` is true in the program's root module (`resolve` is std's, velt:import_meta).
//! They name the source file, fixed while compiling.

use std::path::Path;

use velt_syntax::ast;

/// Fills in the fields of `module`'s `import.meta` constant, if it has one, for the file `path`
/// (the program's root module if `main`).
pub(super) fn fill(module: &mut ast::Module, path: &Path, main: bool) {
    let Some(init) = module
        .items
        .iter_mut()
        .rev()
        .find_map(|i| match &mut i.kind {
            ast::ItemKind::Var(ast::VarDecl {
                pattern:
                    ast::Pattern {
                        kind: ast::PatternKind::Ident(name),
                        ..
                    },
                init: Some(init),
                ..
            }) if name.name == ast::IMPORT_META => Some(init),
            _ => None,
        })
    else {
        return;
    };
    let [url, filename, dirname] = meta_values(path);
    // `new __ImportMeta(dirname, filename, main, url)`.
    let ast::ExprKind::New { args, .. } = &mut init.kind else {
        return;
    };
    let values = [
        ast::Lit::Str(dirname),
        ast::Lit::Str(filename),
        ast::Lit::Bool(main),
        ast::Lit::Str(url),
    ];
    for (arg, value) in args.iter_mut().zip(values) {
        arg.kind = ast::ExprKind::Lit(value);
    }
}

/// `import.meta`'s `url`, `filename` and `dirname` for the module at `path`: absolute and
/// normalized (`a/../b` is `b`) on every OS, as Node gives them.
fn meta_values(path: &Path) -> [String; 3] {
    let file = vpm::relpath::absolute(path);
    let dir = file.parent().unwrap_or(&file).to_path_buf();
    [
        file_url(&file),
        file.display().to_string(),
        dir.display().to_string(),
    ]
}

/// The `file:` URL of the absolute path `file`, as Node's `url.pathToFileURL` gives it:
/// `/`-separated (a Windows drive path gets a leading `/`), with the bytes a URL path cannot
/// hold percent-encoded.
pub(super) fn file_url(file: &Path) -> String {
    let text = file.display().to_string();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut path = match cfg!(windows) {
        true => text.replace('\\', "/"),
        false => text.to_string(),
    };
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    let mut out = String::from("file://");
    for b in path.bytes() {
        let keep = b.is_ascii_alphanumeric() || b"/-._!$&'()*+,;=:@".contains(&b);
        match keep {
            true => out.push(b as char),
            false => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{file_url, meta_values};
    use std::path::Path;

    #[test]
    fn paths_are_normalized() {
        let [url, filename, dirname] = meta_values(Path::new("a/../b/./m.ts"));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(Path::new(&filename), cwd.join("b").join("m.ts"));
        assert_eq!(Path::new(&dirname), cwd.join("b"));
        assert!(!url.contains("..") && !url.contains("/./"), "{url}");
        assert!(url.ends_with("/b/m.ts"), "{url}");
    }

    #[test]
    fn file_urls() {
        assert_eq!(file_url(Path::new("/a/b c/d.ts")), "file:///a/b%20c/d.ts");
        assert_eq!(
            file_url(Path::new("/x/%#?é.ts")),
            "file:///x/%25%23%3F%C3%A9.ts"
        );
        if cfg!(windows) {
            assert_eq!(file_url(Path::new(r"C:\Dev\m.ts")), "file:///C:/Dev/m.ts");
        }
    }
}

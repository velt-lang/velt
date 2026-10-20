//! `import.meta` (`ast::IMPORT_META`): the parser adds a module constant with placeholder fields
//! to a module that mentions it, and the loader, which knows the module's file, fills them in as
//! Node does: `url` is the file's `file:` URL, `filename` its absolute path and `dirname` its
//! directory, `main` is true in the program's root module, and `resolve` resolves against `url`.
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
    let ast::ExprKind::Object(props) = &mut init.kind else {
        return;
    };
    for p in props {
        let ast::ObjectProp::KeyValue(key, e) = p else {
            continue;
        };
        let text = match key.name.as_str() {
            "url" => url.clone(),
            "filename" => filename.clone(),
            "dirname" => dirname.clone(),
            "main" => {
                e.kind = ast::ExprKind::Lit(ast::Lit::Bool(main));
                continue;
            }
            "resolve" => {
                fill_resolve_base(e, &url);
                continue;
            }
            _ => continue,
        };
        e.kind = ast::ExprKind::Lit(ast::Lit::Str(text));
    }
}

/// Writes `url` into the base argument of `import.meta.resolve`'s arrow (`specifier =>
/// __import_meta_resolve(specifier, "")`).
fn fill_resolve_base(e: &mut ast::Expr, url: &str) {
    let ast::ExprKind::Arrow {
        body: ast::ArrowBody::Expr(call),
        ..
    } = &mut e.kind
    else {
        return;
    };
    if let ast::ExprKind::Call { args, .. } = &mut call.kind {
        if let Some(base) = args.get_mut(1) {
            base.kind = ast::ExprKind::Lit(ast::Lit::Str(url.to_string()));
        }
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

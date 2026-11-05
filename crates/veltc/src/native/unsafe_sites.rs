//! The `unsafe` code of a module (docs/internals/design/c-ffi.md "The trust boundary"): every
//! `unsafe { … }` block and expression and every unsafe function or method, found in its AST,
//! and the members of namespace imports it names (`ffi.Ptr`). The trust check confines them to
//! allowed native packages and std ([`super::trust`]); the `// SAFETY:` lint reads the comment
//! before each ([`safety_comment`]).

use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E, StmtKind as S};
use velt_syntax::visit::{walk_expr, walk_module, walk_pattern, Visit};

/// What kind of unsafe code a site is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SiteKind {
    /// `unsafe { … }` as a statement.
    Block,
    /// `unsafe { expr }` in expression position.
    Expr,
    /// `unsafe function name` or an unsafe method `name`.
    Function(String),
}

/// A piece of unsafe code: what it is, and where (a function's name, a block's start).
#[derive(Clone, Debug)]
pub(super) struct Site {
    pub kind: SiteKind,
    pub span: Span,
}

/// What [`find`] collects from a module.
#[derive(Default)]
pub(super) struct Found {
    /// Its unsafe code, in source order.
    pub sites: Vec<Site>,
    /// `a.b` with `a` a plain name, in expressions and in type paths: the uses of a namespace
    /// import's members (`a` the namespace, `b` the member).
    pub qualified: Vec<(String, ast::Ident)>,
}

/// The unsafe code of `module` and its qualified names. Unlike [`walk_module`] alone, this
/// visits the defaults inside destructuring patterns and arrow parameters too, so no unsafe
/// code is left out.
pub(super) fn find(module: &ast::Module) -> Found {
    let mut f = Finder(Found::default());
    walk_module(module, &mut f);
    f.0
}

struct Finder(Found);

impl Finder {
    fn site(&mut self, kind: SiteKind, span: Span) {
        self.0.sites.push(Site { kind, span });
    }

    fn function_sig(&mut self, sig: &ast::FnSig) {
        if sig.is_unsafe {
            self.site(SiteKind::Function(sig.name.name.clone()), sig.name.span);
        }
    }
}

impl<'a> Visit<'a> for Finder {
    fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
        self.function_sig(sig);
        for p in &sig.params {
            if let Some(pattern) = &p.pattern {
                walk_pattern(pattern, self);
            }
        }
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        match &s.kind {
            S::Unsafe(_) => self.site(SiteKind::Block, s.span),
            S::ForOf { pattern, .. } => walk_pattern(pattern, self),
            S::Try {
                catch: Some((Some(pattern), _)),
                ..
            } => walk_pattern(pattern, self),
            _ => {}
        }
    }

    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        walk_pattern(&v.pattern, self);
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        match &e.kind {
            E::Unsafe(_) => self.site(SiteKind::Expr, e.span),
            E::Arrow { params, .. } => {
                for p in params {
                    if let Some(d) = &p.default {
                        walk_expr(d, self);
                    }
                    if let Some(pattern) = &p.pattern {
                        walk_pattern(pattern, self);
                    }
                }
            }
            E::Member { object, prop, .. } => {
                if let E::Ident(ns) = &object.kind {
                    self.0.qualified.push((ns.name.clone(), prop.clone()));
                }
            }
            _ => {}
        }
    }

    fn ty(&mut self, t: &'a ast::TypeExpr) {
        if let ast::TypeExprKind::Named { path, .. } = &t.kind {
            if let [ns, member, ..] = path.as_slice() {
                self.0.qualified.push((ns.name.clone(), member.clone()));
            }
        }
    }
}

/// Whether the code at byte `lo` of `src` has a safety comment: a `SAFETY:` in the comment
/// lines directly above its line (no blank line between), or for an unsafe function (`function`)
/// also the `Unsafe:` paragraph of its doc comment, which says what callers must guarantee.
pub(super) fn safety_comment(src: &str, lo: usize, function: bool) -> bool {
    let lo = lo.min(src.len());
    let line_start = src[..lo].rfind('\n').map_or(0, |i| i + 1);
    let above = src[..line_start].lines().rev().map(str::trim);
    for line in above {
        let comment = line.starts_with("//") || line.starts_with("/*") || line.starts_with('*');
        if !comment {
            return false;
        }
        if line.contains("SAFETY:") || (function && line.contains("Unsafe:")) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites(src: &str) -> Vec<SiteKind> {
        let (m, d) = velt_syntax::parse_file(velt_common::FileId(0), src);
        assert!(d.is_empty(), "{src}: {d:?}");
        find(&m).sites.into_iter().map(|s| s.kind).collect()
    }

    #[test]
    fn every_unsafe_site_is_found_defaults_included() {
        let src = r#"
unsafe function raw(): i64 { return 1; }
class C { unsafe m(): i64 { return 2; } }
function f(x: i64 = unsafe { raw() }) {
  unsafe { raw(); }
  const { a = unsafe { raw() } } = { a: 1 };
  const g = (y: i64 = unsafe { raw() }) => y;
  for (const [b = unsafe { raw() }] of [[1]]) {}
}
"#;
        use SiteKind::*;
        assert_eq!(
            sites(src),
            vec![
                Function("raw".into()),
                Function("m".into()),
                Expr,
                Block,
                Expr,
                Expr,
                Expr,
            ]
        );
    }

    #[test]
    fn a_safety_comment_is_the_comment_block_directly_above() {
        let src = "// SAFETY: fine.\n// more\nunsafe {}\n\n// SAFETY: far\n\nunsafe {}\n";
        let first = src.find("unsafe").unwrap();
        let second = src.rfind("unsafe").unwrap();
        assert!(safety_comment(src, first, false));
        assert!(!safety_comment(src, second, false));
        let doc = "/**\n * Reads. Unsafe: `p` points at bytes.\n */\nunsafe function r() {}\n";
        let at = doc.find("unsafe function").unwrap();
        assert!(safety_comment(doc, at, true));
        assert!(!safety_comment(doc, at, false));
        assert!(!safety_comment("x();\nunsafe {}", 5, false));
        assert!(!safety_comment("unsafe {}", 0, false));
    }
}

#[cfg(test)]
mod std_tests {
    use super::*;

    fn vlt_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("std is readable") {
            let path = entry.expect("std entry").path();
            if path.is_dir() {
                vlt_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "vlt") {
                out.push(path);
            }
        }
    }

    /// std is the root of trust: its unsafe code follows the packages' `// SAFETY:` rule, which
    /// builds don't check for std (docs/internals/design/c-ffi.md "The trust boundary").
    #[test]
    fn std_s_unsafe_code_has_safety_comments() {
        let std_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
        let mut files = Vec::new();
        vlt_files(&std_root, &mut files);
        files.sort();
        let mut missing = Vec::new();
        for path in &files {
            let src = std::fs::read_to_string(path).expect("std file");
            if !src.contains("unsafe") {
                continue;
            }
            let (module, _) = velt_syntax::parse_file(velt_common::FileId(0), &src);
            for site in find(&module).sites {
                let function = matches!(site.kind, SiteKind::Function(_));
                if !safety_comment(&src, site.span.lo as usize, function) {
                    let line = src[..site.span.lo as usize].lines().count().max(1);
                    let rel = path.strip_prefix(&std_root).unwrap_or(path);
                    missing.push(format!("std/{}:{line}: {:?}", rel.display(), site.kind));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "unsafe code without a `// SAFETY:` comment directly before it (an unsafe function: \
             or an `Unsafe:` paragraph in its doc comment):\n{}",
            missing.join("\n")
        );
    }
}

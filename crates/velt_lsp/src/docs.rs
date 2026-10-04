//! Doc comments of definitions (`/** … */` and `///`, read by [`velt_doc::comment`]) for hover,
//! completion and signature help.
//!
//! A sema [`DefRef`] names its declaring identifier; the declaration it belongs to starts earlier
//! (at `export`, a modifier or the keyword), and the doc comment ends right above that start. Each
//! file's declarations are indexed once: every name of an item, a field, method, constructor or
//! variant of a type, an `extend` member, an interface member and a field of an object type, to
//! where its declaration starts. A parameter is indexed to its function, whose `@param` entry is
//! its doc. Re-exports need nothing: sema's definition is the original declaration.
//!
//! The index and the file's comment ranges are kept per file of the analysis and, by the file's
//! text, across analyses (the standard library's files do not change between edits).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use lsp_types::{CompletionItem, CompletionItemTag};
use velt_common::{FileId, Span};
use velt_doc::comment::{self, DocComment};
use velt_sema::ide::{DefKind, DefRef};
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::pattern_idents;

/// The documented declarations of one file.
#[derive(Default)]
pub struct FileDocs {
    /// The file's comments (`velt_syntax::comment_ranges`).
    comments: Vec<Range<u32>>,
    /// Name span `(lo, hi)` → its declaration.
    decls: HashMap<(u32, u32), Decl>,
}

/// Where the doc of a name is.
#[derive(Clone, Debug)]
enum Decl {
    /// Above the declaration starting here.
    At(u32),
    /// In the `@param` entry `name` of the function declared here.
    Param { decl_lo: u32, name: String },
}

/// The per-analysis cache: file → its index (`None` when the file is not a parsed module).
#[derive(Default)]
pub struct Cache(Mutex<HashMap<FileId, Option<Arc<FileDocs>>>>);

/// Indexes by the hash of a file's text, across analyses. Cleared when it grows past
/// [`SHARED_LIMIT`] (edited documents leave old versions behind).
fn shared() -> &'static Mutex<HashMap<u64, Arc<FileDocs>>> {
    static SHARED: OnceLock<Mutex<HashMap<u64, Arc<FileDocs>>>> = OnceLock::new();
    SHARED.get_or_init(Default::default)
}

const SHARED_LIMIT: usize = 1024;

/// The doc comment of `def`: of its declaration, or for a parameter its function's `@param`
/// entry (as the body). `None` for compiler-provided definitions, locals and undocumented ones.
pub fn doc_for(analysis: &Analysis, def: &DefRef) -> Option<DocComment> {
    if def.span == Span::DUMMY || def.kind == DefKind::Local {
        return None;
    }
    doc_at(analysis, def.span)
}

/// The doc comment of the declaration whose name is at `name`.
pub fn doc_at(analysis: &Analysis, name: Span) -> Option<DocComment> {
    let docs = file_docs(analysis, name.file)?;
    let src = &analysis.sm.get(name.file).src;
    let doc = match docs.decls.get(&(name.lo, name.hi))? {
        Decl::At(lo) => comment::doc_before_in(src, &docs.comments, *lo)?,
        Decl::Param { decl_lo, name } => {
            let doc = comment::doc_before_in(src, &docs.comments, *decl_lo)?;
            DocComment {
                body: doc.param(name)?.to_string(),
                ..Default::default()
            }
        }
    };
    (!doc.is_empty()).then_some(doc)
}

/// The doc to show under a signature: the whole comment rendered as Markdown.
pub fn markdown_for(analysis: &Analysis, def: &DefRef) -> Option<String> {
    doc_for(analysis, def)
        .map(|d| d.render_markdown())
        .filter(|s| !s.is_empty())
}

/// Mark a completion item for `def` for `completionItem/resolve` when `def` is documented (its
/// `data`: the declaring file's path and the name's range; the server adds the document's URI),
/// and tag it deprecated when it is.
pub fn attach(analysis: &Analysis, item: &mut CompletionItem, def: &DefRef) {
    if def.span == Span::DUMMY || def.kind == DefKind::Local {
        return;
    }
    attach_at(analysis, item, def.span);
}

/// [`attach`] for the declaration whose name is at `name`.
pub fn attach_at(analysis: &Analysis, item: &mut CompletionItem, name: Span) {
    let Some(doc) = doc_at(analysis, name) else {
        return;
    };
    let path = analysis.sm.get(name.file).path.to_string_lossy();
    item.data = Some(serde_json::json!({ "path": path, "lo": name.lo, "hi": name.hi }));
    if doc.deprecated.is_some() {
        item.tags = Some(vec![CompletionItemTag::DEPRECATED]);
        item.deprecated = Some(true);
    }
}

/// The doc of a completion item's definition, from the `data` [`attach`] left (`None` if it has
/// none or the file is no longer part of the analysis).
pub fn resolve(analysis: &Analysis, data: &serde_json::Value) -> Option<String> {
    let path = data["path"].as_str()?;
    let lo = u32::try_from(data["lo"].as_u64()?).ok()?;
    let hi = u32::try_from(data["hi"].as_u64()?).ok()?;
    let (file, _) = analysis
        .sm
        .files()
        .find(|(_, f)| f.path.to_string_lossy() == path)?;
    let doc = doc_at(analysis, Span::new(file, lo, hi))?.render_markdown();
    (!doc.is_empty()).then_some(doc)
}

fn file_docs(analysis: &Analysis, file: FileId) -> Option<Arc<FileDocs>> {
    let mut cache = analysis.docs.0.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry(file)
        .or_insert_with(|| build(analysis, file))
        .clone()
}

fn build(analysis: &Analysis, file: FileId) -> Option<Arc<FileDocs>> {
    let module = analysis.modules.iter().find(|m| m.file == file)?;
    let src = &analysis.sm.get(file).src;
    let mut hasher = DefaultHasher::new();
    src.hash(&mut hasher);
    let key = hasher.finish();
    let mut shared = shared().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = shared.get(&key) {
        return Some(found.clone());
    }
    let mut docs = FileDocs {
        comments: velt_syntax::comment_ranges(src),
        decls: HashMap::new(),
    };
    for item in &module.ast.items {
        docs.item(item);
    }
    let docs = Arc::new(docs);
    if shared.len() >= SHARED_LIMIT {
        shared.clear();
    }
    shared.insert(key, docs.clone());
    Some(docs)
}

impl FileDocs {
    fn add(&mut self, name: &ast::Ident, decl_lo: u32) {
        self.decls
            .entry((name.span.lo, name.span.hi))
            .or_insert(Decl::At(decl_lo));
    }

    fn param(&mut self, name: &ast::Ident, decl_lo: u32) {
        self.decls
            .entry((name.span.lo, name.span.hi))
            .or_insert(Decl::Param {
                decl_lo,
                name: name.name.clone(),
            });
    }

    fn item(&mut self, item: &ast::Item) {
        let lo = item.span.lo;
        match &item.kind {
            ast::ItemKind::Import(_) => {}
            ast::ItemKind::Function(f) => self.sig(&f.sig, lo),
            ast::ItemKind::ExternFn(sig) => self.sig(sig, lo),
            ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => {
                self.add(&t.name, lo);
                self.fields(&t.fields);
                if let Some(ctor) = &t.constructor {
                    self.sig(&ctor.sig, ctor.sig.span.lo);
                }
                for m in &t.methods {
                    self.sig(&m.decl.sig, m.decl.sig.span.lo);
                }
            }
            ast::ItemKind::Interface(i) => {
                self.add(&i.name, lo);
                self.fields(&i.fields);
                for m in &i.methods {
                    self.sig(&m.sig, m.sig.span.lo);
                }
            }
            ast::ItemKind::Enum(e) => {
                self.add(&e.name, lo);
                for v in &e.variants {
                    self.add(&v.name, v.span.lo);
                }
            }
            ast::ItemKind::TypeAlias(t) => {
                self.add(&t.name, lo);
                self.type_fields(&t.ty);
            }
            ast::ItemKind::Var(v) => {
                for name in pattern_idents(&v.pattern) {
                    self.add(name, lo);
                }
                if let Some(ty) = &v.ty {
                    self.type_fields(ty);
                }
                if let Some(ast::ExprKind::Arrow { params, .. }) = v.init.as_ref().map(|e| &e.kind)
                {
                    for p in params {
                        self.param(&p.name, lo);
                    }
                }
            }
            ast::ItemKind::Extend(e) => {
                for m in &e.methods {
                    self.sig(&m.decl.sig, m.decl.sig.span.lo);
                }
            }
        }
    }

    /// A function, method or constructor declared at `lo`, and its parameters.
    fn sig(&mut self, sig: &ast::FnSig, lo: u32) {
        self.add(&sig.name, lo);
        for p in &sig.params {
            self.param(&p.name, lo);
            self.type_fields(&p.ty);
        }
    }

    fn fields(&mut self, fields: &[ast::Field]) {
        for f in fields {
            self.add(&f.name, f.span.lo);
            self.type_fields(&f.ty);
        }
    }

    /// The fields of the object types in `ty` (`type Props = { label: string }`).
    fn type_fields(&mut self, ty: &ast::TypeExpr) {
        match &ty.kind {
            ast::TypeExprKind::Named { args, .. } => args.iter().for_each(|t| self.type_fields(t)),
            ast::TypeExprKind::Array(t) => self.type_fields(t),
            ast::TypeExprKind::Tuple(ts) | ast::TypeExprKind::Union(ts) => {
                ts.iter().for_each(|t| self.type_fields(t))
            }
            ast::TypeExprKind::Function { params, ret, .. } => {
                params.iter().for_each(|t| self.type_fields(t));
                self.type_fields(ret);
            }
            ast::TypeExprKind::Object(fields) => {
                for f in fields {
                    self.add(&f.name, f.span.lo);
                    self.type_fields(&f.ty);
                }
            }
            ast::TypeExprKind::Literal(_) | ast::TypeExprKind::Null | ast::TypeExprKind::Void => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"/**
 * Adds.
 * @param a - the first
 * @returns the sum
 */
export function add(a: i64, b: i64): i64 {
  return a + b;
}

/** A user. */
class User {
  /** The name. */
  private name: string;
  /** Makes one. */
  constructor(name: string) {
    this.name = name;
  }
  /**
   * Greets.
   * @deprecated use `hello`
   */
  static greet(): string {
    return "hi";
  }
}

enum Color {
  /** Red. */
  Red,
}

type Props = {
  /** The label. */
  label: string;
};
"#;

    fn docs() -> (Analysis, FileId) {
        let mut sm = velt_common::SourceMap::new();
        let file = sm.add("docs_test.vlt", SRC);
        let (ast, diags) = velt_syntax::parse_file(file, SRC);
        assert!(diags.is_empty(), "{diags:?}");
        let module = velt_sema::SourceModule {
            path: "main".into(),
            is_std: false,
            file,
            ast,
            imports: vec![],
            jsx_runtime: None,
        };
        let analysis = Analysis {
            sm,
            modules: vec![module],
            root: 0,
            diagnostics: vec![],
            ide: None,
            ts_compat: vec![],
            docs: Cache::default(),
        };
        (analysis, file)
    }

    fn doc(analysis: &Analysis, file: FileId, needle: &str) -> Option<DocComment> {
        let lo = SRC.find(needle).unwrap() as u32;
        let name = needle.split(|c: char| !c.is_alphanumeric()).next().unwrap();
        doc_at(analysis, Span::new(file, lo, lo + name.len() as u32))
    }

    #[test]
    fn finds_docs_of_items_members_and_parameters() {
        let (a, f) = docs();
        let add = doc(&a, f, "add(").unwrap();
        assert_eq!(add.body, "Adds.");
        assert_eq!(add.returns.as_deref(), Some("the sum"));
        assert_eq!(doc(&a, f, "a: i64").unwrap().body, "the first");
        assert!(doc(&a, f, "b: i64").is_none());
        assert_eq!(doc(&a, f, "User {").unwrap().body, "A user.");
        assert_eq!(doc(&a, f, "name: string;").unwrap().body, "The name.");
        assert_eq!(doc(&a, f, "constructor(").unwrap().body, "Makes one.");
        let greet = doc(&a, f, "greet(").unwrap();
        assert_eq!(greet.deprecated.as_deref(), Some("use `hello`"));
        assert_eq!(doc(&a, f, "Red,").unwrap().body, "Red.");
        assert_eq!(doc(&a, f, "label:").unwrap().body, "The label.");
        assert!(doc(&a, f, "Color {").is_none());
    }
}

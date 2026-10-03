//! Test-only AST builder: hand-built `ast::Module`s while the real parser is developed in parallel.
//! All nodes get `Span::DUMMY` unless overridden with [`At::at`]; use [`Src::sp`] to compute a real
//! span from the golden source text when a test checks diagnostic positions.
// Each test binary uses a different subset of these shared helpers (and re-exports).
#![allow(dead_code, unused_imports)]

pub mod hir_walk;
pub mod process_work;
pub mod programs;

use std::cell::Cell;
use std::path::PathBuf;

use velt_common::{Diagnostics, FileId, Span};
use velt_sema::{check_with, hir, CheckOptions, SourceModule};
use velt_syntax::ast::*;

pub use velt_syntax::ast::BinaryOp as B;

thread_local!(static NEXT: Cell<u32> = const { Cell::new(0) });

fn nid() -> NodeId {
    NEXT.with(|n| {
        let v = n.get();
        n.set(v + 1);
        NodeId(v)
    })
}

pub const D: Span = Span::DUMMY;

// ─────────────── spans from source text ───────────────

pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/m1")
}

pub struct Src {
    pub text: String,
}

impl Src {
    pub fn load(rel: &str) -> Src {
        let p = golden_dir().join(rel);
        let text =
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        Src {
            text: text.replace("\r\n", "\n"),
        }
    }

    /// Span of the `nth` (0-based) occurrence of `needle`.
    pub fn sp(&self, needle: &str, nth: usize) -> Span {
        let mut from = 0;
        for _ in 0..nth {
            from += self.text[from..].find(needle).expect("needle") + needle.len();
        }
        let lo = from
            + self.text[from..]
                .find(needle)
                .unwrap_or_else(|| panic!("needle `{needle}` not found"));
        Span::new(FileId(0), lo as u32, (lo + needle.len()) as u32)
    }

    /// Byte offset of 1-based line:col.
    pub fn offset(&self, line: usize, col: usize) -> u32 {
        let mut off = 0;
        for (i, l) in self.text.split('\n').enumerate() {
            if i + 1 == line {
                return (off + col - 1) as u32;
            }
            off += l.len() + 1;
        }
        panic!("line {line} out of range")
    }
}

pub trait At {
    fn at(self, sp: Span) -> Self;
}

impl At for Expr {
    fn at(mut self, sp: Span) -> Self {
        self.span = sp;
        if let ExprKind::Ident(i) = &mut self.kind {
            i.span = sp;
        }
        self
    }
}

impl At for Stmt {
    fn at(mut self, sp: Span) -> Self {
        self.span = sp;
        self
    }
}

// ─────────────── idents & types ───────────────

pub fn ident(name: &str) -> Ident {
    Ident {
        name: name.to_string(),
        span: D,
    }
}

pub fn ty(name: &str) -> TypeExpr {
    let kind = if name == "void" {
        TypeExprKind::Void
    } else {
        TypeExprKind::Named {
            path: vec![ident(name)],
            args: vec![],
        }
    };
    TypeExpr { kind, span: D }
}

// ─────────────── expressions ───────────────

pub fn ex(kind: ExprKind) -> Expr {
    Expr {
        id: nid(),
        kind,
        span: D,
    }
}

pub fn int(v: u128) -> Expr {
    ex(ExprKind::Lit(Lit::Int {
        value: v,
        suffix: None,
    }))
}

pub fn int_s(v: u128, suffix: &str) -> Expr {
    ex(ExprKind::Lit(Lit::Int {
        value: v,
        suffix: Some(suffix.into()),
    }))
}

pub fn float(v: f64) -> Expr {
    ex(ExprKind::Lit(Lit::Float {
        value: v,
        suffix: None,
    }))
}

pub fn str_(s: &str) -> Expr {
    ex(ExprKind::Lit(Lit::Str(s.into())))
}

pub fn bool_(b: bool) -> Expr {
    ex(ExprKind::Lit(Lit::Bool(b)))
}

pub fn var(name: &str) -> Expr {
    ex(ExprKind::Ident(ident(name)))
}

pub fn bin(op: BinaryOp, a: Expr, b: Expr) -> Expr {
    ex(ExprKind::Binary {
        op,
        lhs: Box::new(a),
        rhs: Box::new(b),
    })
}

pub fn neg(a: Expr) -> Expr {
    ex(ExprKind::Unary {
        op: UnaryOp::Neg,
        expr: Box::new(a),
    })
}

pub fn not(a: Expr) -> Expr {
    ex(ExprKind::Unary {
        op: UnaryOp::Not,
        expr: Box::new(a),
    })
}

pub fn bitnot(a: Expr) -> Expr {
    ex(ExprKind::Unary {
        op: UnaryOp::BitNot,
        expr: Box::new(a),
    })
}

pub fn assign(target: Expr, value: Expr) -> Expr {
    ex(ExprKind::Assign {
        op: None,
        target: Box::new(target),
        value: Box::new(value),
    })
}

pub fn cassign(op: BinaryOp, target: Expr, value: Expr) -> Expr {
    ex(ExprKind::Assign {
        op: Some(op),
        target: Box::new(target),
        value: Box::new(value),
    })
}

pub fn post_inc(target: Expr) -> Expr {
    ex(ExprKind::Update {
        op: UpdateOp::Inc,
        prefix: false,
        target: Box::new(target),
    })
}

pub fn post_dec(target: Expr) -> Expr {
    ex(ExprKind::Update {
        op: UpdateOp::Dec,
        prefix: false,
        target: Box::new(target),
    })
}

pub fn pre_inc(target: Expr) -> Expr {
    ex(ExprKind::Update {
        op: UpdateOp::Inc,
        prefix: true,
        target: Box::new(target),
    })
}

pub fn cond(c: Expr, a: Expr, b: Expr) -> Expr {
    ex(ExprKind::Cond {
        cond: Box::new(c),
        then: Box::new(a),
        els: Box::new(b),
    })
}

pub fn call_e(callee: Expr, args: Vec<Expr>) -> Expr {
    ex(ExprKind::Call {
        callee: Box::new(callee),
        type_args: vec![],
        args,
        optional: false,
    })
}

pub fn call(name: &str, args: Vec<Expr>) -> Expr {
    call_e(var(name), args)
}

pub fn member(obj: Expr, prop: &str) -> Expr {
    ex(ExprKind::Member {
        object: Box::new(obj),
        prop: ident(prop),
        optional: false,
    })
}

pub fn log(args: Vec<Expr>) -> Expr {
    call_e(member(var("console"), "log"), args)
}

pub fn cast(e: Expr, t: &str) -> Expr {
    ex(ExprKind::Cast {
        expr: Box::new(e),
        ty: ty(t),
    })
}

pub fn paren(e: Expr) -> Expr {
    ex(ExprKind::Paren(Box::new(e)))
}

pub fn tpl(quasis: &[&str], exprs: Vec<Expr>) -> Expr {
    assert_eq!(quasis.len(), exprs.len() + 1);
    ex(ExprKind::Template {
        quasis: quasis.iter().map(|s| s.to_string()).collect(),
        exprs,
    })
}

// ─────────────── statements ───────────────

pub fn st(kind: StmtKind) -> Stmt {
    Stmt { kind, span: D }
}

fn decl(kind: VarKind, name: &str, t: Option<&str>, init: Option<Expr>) -> Stmt {
    let pattern = Pattern {
        id: nid(),
        kind: PatternKind::Ident(ident(name)),
        span: D,
    };
    st(StmtKind::Var(VarDecl {
        kind,
        pattern,
        ty: t.map(ty),
        init,
        span: D,
    }))
}

pub fn let_(name: &str, init: Expr) -> Stmt {
    decl(VarKind::Let, name, None, Some(init))
}

pub fn let_t(name: &str, t: &str, init: Option<Expr>) -> Stmt {
    decl(VarKind::Let, name, Some(t), init)
}

pub fn const_(name: &str, init: Expr) -> Stmt {
    decl(VarKind::Const, name, None, Some(init))
}

pub fn const_t(name: &str, t: &str, init: Expr) -> Stmt {
    decl(VarKind::Const, name, Some(t), Some(init))
}

pub fn es(e: Expr) -> Stmt {
    st(StmtKind::Expr(e))
}

pub fn ret(e: Expr) -> Stmt {
    st(StmtKind::Return(Some(e)))
}

pub fn ret_void() -> Stmt {
    st(StmtKind::Return(None))
}

pub fn blk(stmts: Vec<Stmt>) -> Block {
    Block { stmts, span: D }
}

pub fn block_s(stmts: Vec<Stmt>) -> Stmt {
    st(StmtKind::Block(blk(stmts)))
}

pub fn if_(c: Expr, then: Vec<Stmt>, els: Option<Stmt>) -> Stmt {
    st(StmtKind::If {
        cond: c,
        then: blk(then),
        els: els.map(Box::new),
    })
}

pub fn while_(c: Expr, body: Vec<Stmt>) -> Stmt {
    st(StmtKind::While {
        cond: c,
        body: blk(body),
    })
}

pub fn do_while(body: Vec<Stmt>, c: Expr) -> Stmt {
    st(StmtKind::DoWhile {
        body: blk(body),
        cond: c,
    })
}

pub fn for_(init: Option<Stmt>, c: Option<Expr>, update: Option<Expr>, body: Vec<Stmt>) -> Stmt {
    st(StmtKind::For {
        init: init.map(Box::new),
        cond: c,
        update,
        body: blk(body),
    })
}

pub fn brk(label: Option<&str>) -> Stmt {
    st(StmtKind::Break(label.map(ident)))
}

pub fn cont(label: Option<&str>) -> Stmt {
    st(StmtKind::Continue(label.map(ident)))
}

pub fn labeled(label: &str, body: Stmt) -> Stmt {
    st(StmtKind::Labeled {
        label: ident(label),
        body: Box::new(body),
    })
}

// ─────────────── items & programs ───────────────

pub fn func(name: &str, params: &[(&str, &str)], ret: Option<&str>, body: Vec<Stmt>) -> Item {
    let params = params
        .iter()
        .map(|(n, t)| Param {
            name: ident(n),
            ty: ty(t),
            default: None,
            optional: false,
            span: D,
        })
        .collect();
    let sig = FnSig {
        name: ident(name),
        generics: vec![],
        params,
        ret: ret.map(ty),
        throws: None,
        is_async: false,
        span: D,
    };
    Item {
        kind: ItemKind::Function(FnDecl {
            sig,
            body: blk(body),
        }),
        exported: false,
        span: D,
    }
}

pub fn run(items: Vec<Item>) -> (Option<hir::Program>, Diagnostics) {
    run_with(items, CheckOptions::default())
}

/// [`run`] with check options (e.g. a library module without `main`).
pub fn run_with(items: Vec<Item>, opts: CheckOptions) -> (Option<hir::Program>, Diagnostics) {
    let m = SourceModule {
        path: "main".into(),
        is_std: false,
        file: FileId(0),
        ast: Module {
            items,
            span: D,
            jsx_import_source: None,
        },
        imports: vec![],
        jsx_runtime: None,
    };
    check_with(&[m], 0, opts)
}

/// Check and require success.
pub fn ok(items: Vec<Item>) -> hir::Program {
    let (p, d) = run(items);
    let msgs: Vec<_> = d
        .iter()
        .map(|d| {
            format!(
                "{} @{}..{} {:?}",
                d.message, d.labels[0].span.lo, d.labels[0].span.hi, d.notes
            )
        })
        .collect();
    assert!(
        d.iter().all(|d| !d.is_error()),
        "unexpected diagnostics: {msgs:#?}"
    );
    p.expect("program")
}

/// Check and require failure; returns the error messages.
pub fn errs(items: Vec<Item>) -> Diagnostics {
    let (p, d) = run(items);
    assert!(p.is_none(), "expected errors, program was accepted");
    assert!(!d.is_empty());
    d
}

pub fn has_err(d: &Diagnostics, needle: &str) -> bool {
    d.iter().any(|d| d.message.contains(needle))
}

// ─────────────── HIR inspection ───────────────

pub fn fn_named<'p>(p: &'p hir::Program, name: &str) -> &'p hir::FnDef {
    p.defs
        .iter()
        .find_map(|d| match d {
            hir::Def::Fn(f) if f.name == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no fn {name}"))
}

/// Visit every expression in a block (pre-order).
pub fn walk_block(b: &hir::Block, f: &mut dyn FnMut(&hir::Expr)) {
    for s in &b.stmts {
        walk_stmt(s, f);
    }
    if let Some(v) = &b.value {
        walk_expr(v, f);
    }
}

pub fn walk_stmt(s: &hir::Stmt, f: &mut dyn FnMut(&hir::Expr)) {
    use hir::StmtKind as S;
    match &s.kind {
        S::Let { init, .. } => {
            if let Some(e) = init {
                walk_expr(e, f)
            }
        }
        S::LetPat { init, .. } => walk_expr(init, f),
        S::Expr(e) => walk_expr(e, f),
        S::Return(e) => {
            if let Some(e) = e {
                walk_expr(e, f)
            }
        }
        S::If { cond, then, els } => {
            walk_expr(cond, f);
            walk_block(then, f);
            if let Some(b) = els {
                walk_block(b, f)
            }
        }
        S::While {
            cond, body, step, ..
        } => {
            walk_expr(cond, f);
            walk_block(body, f);
            if let Some(e) = step {
                walk_expr(e, f)
            }
        }
        S::ForOf { iter, body, .. } => {
            walk_expr(iter, f);
            walk_block(body, f);
        }
        S::Break(_) | S::Continue(_) | S::Try { .. } => {}
        S::Block(b) => walk_block(b, f),
    }
}

pub fn walk_expr(e: &hir::Expr, f: &mut dyn FnMut(&hir::Expr)) {
    use hir::ExprKind as E;
    f(e);
    match &e.kind {
        E::Unary { expr, .. } | E::Cast(expr) => walk_expr(expr, f),
        E::Binary { lhs, rhs, .. } | E::Logical { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
            walk_expr(place, f);
            walk_expr(value, f);
        }
        E::Call { args, .. } => args.iter().for_each(|a| walk_expr(a, f)),
        E::If { cond, then, els } => {
            walk_expr(cond, f);
            walk_expr(then, f);
            walk_expr(els, f);
        }
        E::Block(b) => walk_block(b, f),
        _ => {}
    }
}

/// All expressions of a function, pre-order.
pub fn exprs_of(f: &hir::FnDef) -> Vec<hir::Expr> {
    let mut v = vec![];
    walk_block(&f.body.block, &mut |e| v.push(e.clone()));
    v
}

/// Every `(local name, use mode)` read in a function, in order.
pub fn uses(f: &hir::FnDef) -> Vec<(String, hir::UseMode)> {
    exprs_of(f)
        .iter()
        .filter_map(|e| match e.kind {
            hir::ExprKind::Local(l, m) => Some((f.body.locals[l.0 as usize].name.clone(), m)),
            _ => None,
        })
        .collect()
}

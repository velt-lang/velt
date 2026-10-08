//! Locals declared from integer literals (design #525, "Literals and unannotated locals").
//!
//! `let i = 0` declares a `number`, as in TypeScript, unless the function uses `i` only with
//! one declared integer type `T` and never as a number: then `i` is a `T`, as if declared
//! `let i: T = 0`. So `let steps = 0; … return steps` in a function returning `i64` counts in
//! an `i64`. Only a type the program names counts: the integer parameters of the JS API
//! (`s.slice(k)`) and indexes (`xs[k]`) are neutral, so TS-shared code, which never names an
//! integer type, is unaffected (`numrep` proves where its numbers can be integers).
//!
//! The candidates are the unannotated locals whose initializer is built from integer literals
//! and other candidates with `+ - * %`, unary minus and `c ? a : b`. Every use of a candidate,
//! or of such an expression of candidates (`acc + i`), is evidence:
//! - **as `T`**: converted to the declared integer type `T` (assigned, passed, returned or
//!   stored), or combined or compared with a value of a 64-bit integer type `T` (bitwise
//!   operators included: `key | b` with `b: i64`);
//! - **as a number**: next to another number (a float literal, `xs.length`, a `number`
//!   parameter), converted to a float type, an operand of `/` or `**`, the receiver of a
//!   method, or an operand of a bitwise operator (`| & ^ << >> >>> ~`) whose other operand is
//!   not a 64-bit integer (JS's 32-bit semantics: `x << 40` is `x << 8`);
//! - **in arithmetic** (`+ - * %`, unary `-`, `+=`, `++`): a `T` narrower than 64 bits would
//!   wrap where a number does not (`let n = 200; takeU8(n); n + n` is 400), so such a
//!   candidate stays a number, and the use as `T` is an error with a fix-it;
//! - neutral otherwise: next to another candidate (they get the same type), a literal, an
//!   integer type that converts to a number exactly (`i32`), printed, in a template literal,
//!   converted with `as`, passed to an integer parameter of the JS API, an array index.
//!
//! Uses need checked types (the parameter of `m.set` on a `Map<i64, i64>`, a closure's
//! parameter), so the body is checked with every candidate a number, recording the uses; when a
//! candidate resolves to `T`, the body is checked again with it declared `T` (`driver`,
//! `recheck`). A candidate used both ways stays a number: the uses as `T` are then type errors,
//! which get a note pointing at the use as a number.

use std::collections::{HashMap, HashSet};

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, LocalKind};
use crate::ctx::Ctx;
use crate::hir::{self, BinOp, DefId, ExprKind as H, LocalId, TyId, UnOp};

/// The candidates of one check of a body and their uses.
#[derive(Default)]
pub(crate) struct LiteralLocals {
    /// Types decided by an earlier check of this body, by the local's declaration span.
    pub decided: HashMap<Span, TyId>,
    /// The candidates of this check, by declaration span: an index into `parent` / `uses`.
    index: HashMap<Span, usize>,
    /// Union-find: candidates that are used together get one type.
    parent: Vec<usize>,
    uses: Vec<Uses>,
    /// Integer literals checked as numbers (they may be `T` instead).
    int_literals: HashSet<Span>,
}

/// The uses of one candidate (merged into its class's root).
#[derive(Default, Clone)]
struct Uses {
    name: String,
    decl: Span,
    /// Uses as a declared integer type, in order.
    ints: Vec<(TyId, Span)>,
    /// The first use as a number.
    number: Option<Span>,
    /// A negative literal is stored in it: it cannot be unsigned.
    negative: bool,
    /// It takes part in arithmetic: a narrow integer type would wrap.
    arith: bool,
}

impl LiteralLocals {
    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        self.parent[b] = a;
        let ints = std::mem::take(&mut self.uses[b].ints);
        let number = self.uses[b].number.take();
        let negative = std::mem::take(&mut self.uses[b].negative);
        let arith = std::mem::take(&mut self.uses[b].arith);
        let root = &mut self.uses[a];
        root.ints.extend(ints);
        root.negative |= negative;
        root.arith |= arith;
        root.number = match (root.number, number) {
            (Some(x), Some(y)) => Some(if (y.file, y.lo) < (x.file, x.lo) {
                y
            } else {
                x
            }),
            (x, y) => x.or(y),
        };
    }

    /// The candidates `cands` (from `scan`) are used as `T` at `at`.
    fn use_int(&mut self, cands: &[usize], t: TyId, at: Span) {
        for &c in cands {
            let r = self.find(c);
            self.uses[r].ints.push((t, at));
        }
    }

    fn use_number(&mut self, cands: &[usize], at: Span) {
        for &c in cands {
            let r = self.find(c);
            self.uses[r].number.get_or_insert(at);
        }
    }

    fn use_arith(&mut self, cands: &[usize]) {
        for &c in cands {
            let r = self.find(c);
            self.uses[r].arith = true;
        }
    }

    fn join(&mut self, cands: &[usize]) {
        for w in cands.windows(2) {
            self.union(w[0], w[1]);
        }
    }

    fn mark_negative(&mut self, cands: &[usize]) {
        for &c in cands {
            let r = self.find(c);
            self.uses[r].negative = true;
        }
    }

    /// The type each class resolves to: `T` when every use as an integer is `T`, none is as a
    /// number, `T` holds its literals (a negative one is never unsigned) and, unless `T` has
    /// 64 bits, the class takes no part in arithmetic.
    fn resolved(&mut self, ty: &crate::types::Types) -> Vec<(Span, Option<TyId>, Uses)> {
        let mut out = vec![];
        for i in 0..self.parent.len() {
            let r = self.find(i);
            let u = self.uses[r].clone();
            let fits = |t: TyId| {
                let it = ty.int_ty(t);
                (!u.negative || it.is_some_and(|it| it.is_signed()))
                    && (!u.arith || it.is_some_and(|it| it.bits() == 64))
            };
            let t = match u.ints.first() {
                Some(&(t, _))
                    if u.number.is_none() && u.ints.iter().all(|(x, _)| *x == t) && fits(t) =>
                {
                    Some(t)
                }
                _ => None,
            };
            let decl = self.uses[i].decl;
            let mut own = u;
            own.decl = decl;
            own.name.clone_from(&self.uses[i].name);
            out.push((decl, t, own));
        }
        out
    }
}

/// What the last check of `def`'s body found: decide the candidates that resolved to an
/// integer type. Returns whether there are new decisions (the body is checked again).
pub(crate) fn decide(cx: &mut Ctx, def: DefId) -> bool {
    let Some(mut found) = cx.literal_found.remove(&def) else {
        return false;
    };
    let mut new = false;
    let decided = cx.literal_types.entry(def).or_default();
    for (decl, t, _) in found.resolved(&cx.ty) {
        if let Some(t) = t {
            new |= decided.insert(decl, t).is_none();
        }
    }
    cx.literal_found.insert(def, found);
    new
}

/// After the last check of `def`'s body: the type errors at uses as `T` of a candidate that
/// stayed a number (used as one too) get a note saying why, with the fix-it. `diags_from`:
/// the body's first diagnostic.
pub(crate) fn finish(cx: &mut Ctx, def: DefId, diags_from: usize) {
    let Some(mut found) = cx.literal_found.remove(&def) else {
        return;
    };
    for (_, t, uses) in found.resolved(&cx.ty) {
        if t.is_some() || uses.ints.is_empty() {
            continue;
        }
        for (ty, at) in &uses.ints {
            let shown = cx.display(*ty);
            let from = diags_from.min(cx.diags.len());
            let Some(d) = cx.diags[from..]
                .iter_mut()
                .find(|d| d.labels.first().is_some_and(|l| within(l.span, *at)))
            else {
                continue;
            };
            if d.notes.iter().any(|n| n.contains("is a `number`")) {
                continue;
            }
            let why = match uses.number {
                Some(number) => {
                    d.labels.push(velt_common::Label {
                        span: number,
                        message: "used as a `number` here".into(),
                    });
                    "and also used as a `number`".to_string()
                }
                None if uses.negative && !cx.ty.int_ty(*ty).is_some_and(|i| i.is_signed()) => {
                    format!("and holds a negative value, which `{shown}` cannot")
                }
                None if uses.arith && !cx.ty.int_ty(*ty).is_some_and(|i| i.bits() == 64) => {
                    format!("and used in arithmetic, which would wrap at the width of `{shown}`")
                }
                None => "and used with different integer types".to_string(),
            };
            d.notes.push(format!(
                "`{}` is a `number`: it is declared from a literal without a type, {why}",
                uses.name
            ));
            d.notes.push(format!(
                "declare it with the type it needs (`let {}: {shown} = …`; `/` on it is then integer division), or convert here with `as {shown}`",
                uses.name
            ));
        }
    }
    cx.literal_types.remove(&def);
}

/// Does `h` contain a negated literal (`-1`, `x * -2`)?
fn has_negative_literal(h: &hir::Expr) -> bool {
    match &h.kind {
        H::Unary {
            op: UnOp::Neg,
            expr,
        } => matches!(expr.kind, H::Lit(_)) || has_negative_literal(expr),
        H::Binary { lhs, rhs, .. } => has_negative_literal(lhs) || has_negative_literal(rhs),
        H::If { then, els, .. } => has_negative_literal(then) || has_negative_literal(els),
        _ => false,
    }
}

/// Is `inner` inside `outer`?
fn within(inner: Span, outer: Span) -> bool {
    inner.file == outer.file && outer.lo <= inner.lo && inner.hi <= outer.hi
}

impl FnCx<'_, '_> {
    /// The declaration span of local `l` (through the captures of enclosing closures).
    fn decl_span(&self, l: LocalId) -> Span {
        let mut id = l;
        let mut depth = self.outer.len();
        loop {
            let f = if depth == self.outer.len() {
                &self.f
            } else {
                &self.outer[depth]
            };
            if f.kinds[id.0 as usize] == LocalKind::Capture && depth > 0 {
                if let Some(c) = f.captures.iter().find(|c| c.inner == id) {
                    id = c.outer;
                    depth -= 1;
                    continue;
                }
            }
            return f.locals[id.0 as usize].span;
        }
    }

    /// Is `h` built from integer literals and candidates (`acc + i * 2`)? The candidates in it.
    fn scan(&self, h: &hir::Expr) -> Option<Vec<usize>> {
        if h.ty != self.cx.ty.f64 {
            return None;
        }
        let mut out = vec![];
        self.scan_into(h, &mut out).then_some(out)
    }

    fn scan_into(&self, h: &hir::Expr, out: &mut Vec<usize>) -> bool {
        match &h.kind {
            H::Local(l, _) => match self.literal.index.get(&self.decl_span(*l)) {
                Some(&i) => {
                    out.push(i);
                    true
                }
                None => false,
            },
            H::Lit(hir::Lit::Float(_)) => self.literal.int_literals.contains(&h.span),
            H::Unary {
                op: UnOp::Neg,
                expr,
            } => self.scan_into(expr, out),
            H::Binary {
                op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Rem,
                lhs,
                rhs,
            } => self.scan_into(lhs, out) && self.scan_into(rhs, out),
            H::If { then, els, .. } => self.scan_into(then, out) && self.scan_into(els, out),
            _ => false,
        }
    }

    /// An integer literal at `span` was checked as a number.
    pub(crate) fn literal_number_lit(&mut self, span: Span) {
        self.literal.int_literals.insert(span);
    }

    /// The type decided for the unannotated local declared at `name` (by an earlier check).
    pub(crate) fn literal_decided(&self, name: Span) -> Option<TyId> {
        self.literal.decided.get(&name).copied()
    }

    /// `let x = init` without a type declared `local`: a candidate when `init` is built from
    /// literals and candidates.
    pub(crate) fn literal_decl(&mut self, local: LocalId, name: &str, init: &hir::Expr) {
        let Some(cands) = self.scan(init) else {
            return;
        };
        let decl = self.f.locals[local.0 as usize].span;
        let i = self.literal.parent.len();
        self.literal.parent.push(i);
        self.literal.uses.push(Uses {
            name: name.to_string(),
            decl,
            ..Default::default()
        });
        self.literal.index.insert(decl, i);
        if has_negative_literal(init) {
            self.literal.mark_negative(&[i]);
        }
        for c in cands {
            self.literal.union(i, c);
        }
    }

    /// `h` is converted to type `t` (assigned, passed, returned, stored).
    pub(crate) fn literal_use_as(&mut self, h: &hir::Expr, t: TyId) {
        let Some(cands) = self.scan(h).filter(|c| !c.is_empty()) else {
            return;
        };
        let core = self.cx.ty.opt_payload(t).unwrap_or(t);
        if self.cx.ty.is_int(core) {
            self.literal.use_int(&cands, core, h.span);
        } else if self.cx.ty.is_float(core) {
            self.literal.use_number(&cands, h.span);
        }
    }

    /// `h` takes part in arithmetic (`h++`, `-h`).
    pub(crate) fn literal_use_arith(&mut self, h: &hir::Expr) {
        if let Some(cands) = self.scan(h) {
            self.literal.use_arith(&cands);
        }
    }

    /// `h` is used as a number (an operand of `/`, a method's receiver).
    pub(crate) fn literal_use_number(&mut self, h: &hir::Expr) {
        if let Some(cands) = self.scan(h) {
            self.literal.use_number(&cands, h.span);
        }
    }

    /// `l` and `r` are the operands of an arithmetic or comparison operator.
    pub(crate) fn literal_combine(&mut self, l: &hir::Expr, r: &hir::Expr) {
        let (a, b) = (self.scan(l), self.scan(r));
        match (a, b) {
            (Some(mut a), Some(b)) => {
                a.extend(b);
                self.literal.join(&a);
            }
            (Some(c), None) => self.literal_next_to(&c, r, l.span.to(r.span)),
            (None, Some(c)) => self.literal_next_to(&c, l, l.span.to(r.span)),
            (None, None) => {}
        }
    }

    /// Candidates `cands` (in an operand at `at`) next to the other operand `other`.
    fn literal_next_to(&mut self, cands: &[usize], other: &hir::Expr, at: Span) {
        let ty = &self.cx.ty;
        if other.ty == ty.f64 {
            self.literal.use_number(cands, at);
        } else if ty.int_ty(other.ty).is_some_and(|i| i.bits() == 64) {
            self.literal.use_int(cands, other.ty, at);
        }
    }

    /// `l op r` or `l op= r`: the uses of the operands.
    pub(crate) fn literal_operands(&mut self, op: ast::BinaryOp, l: &hir::Expr, r: &hir::Expr) {
        use ast::BinaryOp as B;
        match op {
            B::Div | B::Pow => {
                self.literal_use_number(l);
                self.literal_use_number(r);
            }
            B::And | B::Or | B::Nullish | B::In => {}
            B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr | B::UShr => {
                let wide = |h: &hir::Expr| self.cx.ty.int_ty(h.ty).is_some_and(|i| i.bits() == 64);
                if wide(l) || wide(r) {
                    self.literal_combine(l, r);
                } else {
                    self.literal_use_number(l);
                    self.literal_use_number(r);
                }
            }
            B::Add | B::Sub | B::Mul | B::Rem => {
                self.literal_use_arith(l);
                self.literal_use_arith(r);
                self.literal_combine(l, r);
            }
            _ => self.literal_combine(l, r),
        }
    }

    /// `place = value` where `place` is a candidate: like the two operands of an operator.
    pub(crate) fn literal_assign(&mut self, place: &hir::Expr, value: &hir::Expr) -> bool {
        let Some(mut a) = self.scan(place) else {
            return false;
        };
        if has_negative_literal(value) {
            self.literal.mark_negative(&a);
        }
        match self.scan(value) {
            Some(b) => {
                a.extend(b);
                self.literal.join(&a);
            }
            None => self.literal_next_to(&a, value, value.span),
        }
        true
    }

    /// What this check found, for `decide` and `finish` (`driver`).
    pub(crate) fn literal_take(&mut self) -> LiteralLocals {
        std::mem::take(&mut self.literal)
    }
}

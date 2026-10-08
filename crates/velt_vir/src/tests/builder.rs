//! Test-only helpers to hand-construct HIR the way sema will produce it.

use velt_common::Span;
use velt_sema::hir::*;

pub(super) const SP: Span = Span::DUMMY;

/// Pre-interned common types.
#[derive(Clone, Copy)]
pub(super) struct T {
    pub i32: TyId,
    pub i64: TyId,
    pub u8: TyId,
    pub usize: TyId,
    pub f64: TyId,
    pub bool: TyId,
    pub str: TyId,
    pub unit: TyId,
    pub never: TyId,
}

pub(super) struct PB {
    pub types: TyTable,
    pub defs: Vec<Option<Def>>,
    pub t: T,
    pub entry: Option<DefId>,
    pub impls: Vec<ImplDef>,
}

impl PB {
    pub fn new() -> Self {
        let mut types = TyTable::new();
        let t = T {
            i32: types.intern(TyKind::Int(IntTy::I32)),
            i64: types.intern(TyKind::Int(IntTy::I64)),
            u8: types.intern(TyKind::Int(IntTy::U8)),
            usize: types.intern(TyKind::Int(IntTy::USize)),
            f64: types.intern(TyKind::Float(FloatTy::F64)),
            bool: types.intern(TyKind::Bool),
            str: types.intern(TyKind::Str),
            unit: types.intern(TyKind::Unit),
            never: types.intern(TyKind::Never),
        };
        PB {
            types,
            defs: vec![],
            t,
            entry: None,
            impls: vec![],
        }
    }

    /// Reserve a DefId (for forward references / recursion).
    pub fn declare(&mut self) -> DefId {
        self.defs.push(None);
        DefId(self.defs.len() as u32 - 1)
    }

    pub fn define(&mut self, id: DefId, f: FnDef) {
        self.defs[id.0 as usize] = Some(Def::Fn(f));
    }

    pub fn add_fn(&mut self, f: FnDef) -> DefId {
        let id = self.declare();
        self.define(id, f);
        id
    }

    pub fn add_main(&mut self, f: FnDef) -> DefId {
        let id = self.add_fn(f);
        self.entry = Some(id);
        id
    }

    pub fn finish(self) -> Program {
        Program {
            types: self.types,
            defs: self
                .defs
                .into_iter()
                .map(|d| d.expect("declared but not defined"))
                .collect(),
            entry: self.entry,
            impls: self.impls,
            anon_shapes: Default::default(),
            union_shapes: Default::default(),
        }
    }
}

pub(super) struct FB {
    name: String,
    locals: Vec<LocalDef>,
    params: Vec<Param>,
    ret: TyId,
    pub generics: u32,
    pub self_ty: Option<TyId>,
    pub throws: Option<TyId>,
    pub captures: Vec<Capture>,
}

impl FB {
    pub fn new(name: &str, ret: TyId) -> Self {
        FB {
            name: name.into(),
            locals: vec![],
            params: vec![],
            ret,
            generics: 0,
            self_ty: None,
            throws: None,
            captures: vec![],
        }
    }

    pub fn param(&mut self, name: &str, ty: TyId, mode: PassMode) -> LocalId {
        assert_eq!(
            self.locals.len(),
            self.params.len(),
            "params must be declared first"
        );
        let id = self.local(name, ty);
        self.params.push(Param {
            local: id,
            ty,
            mode,
        });
        id
    }

    pub fn local(&mut self, name: &str, ty: TyId) -> LocalId {
        self.locals.push(LocalDef {
            name: name.into(),
            ty,
            mutable: true,
            boxed: false,
            span: SP,
        });
        LocalId(self.locals.len() as u32 - 1)
    }

    /// Declare local `id` immutable (a param the body never writes).
    pub fn immutable(&mut self, id: LocalId) {
        self.locals[id.0 as usize].mutable = false;
    }

    pub fn get(&self, id: LocalId, mode: UseMode) -> Expr {
        ex(ExprKind::Local(id, mode), self.locals[id.0 as usize].ty)
    }
    pub fn cp(&self, id: LocalId) -> Expr {
        self.get(id, UseMode::Copy)
    }
    pub fn bw(&self, id: LocalId) -> Expr {
        self.get(id, UseMode::Borrow)
    }
    pub fn bm(&self, id: LocalId) -> Expr {
        self.get(id, UseMode::BorrowMut)
    }
    pub fn mv(&self, id: LocalId) -> Expr {
        self.get(id, UseMode::Move)
    }

    pub fn build(self, stmts: Vec<Stmt>) -> FnDef {
        FnDef {
            name: self.name,
            generics: self.generics,
            params: self.params,
            ret: self.ret,
            is_async: false,
            is_generator: false,
            throws: self.throws,
            self_ty: self.self_ty,
            captures: self.captures,
            shares_captures: false,
            body: Body {
                locals: self.locals,
                block: block(stmts),
            },
            span: SP,
        }
    }
}

pub(super) fn ex(kind: ExprKind, ty: TyId) -> Expr {
    Expr { kind, ty, span: SP }
}
pub(super) fn int(v: u128, ty: TyId) -> Expr {
    ex(ExprKind::Lit(Lit::Int(v)), ty)
}
pub(super) fn flt(v: f64, ty: TyId) -> Expr {
    ex(ExprKind::Lit(Lit::Float(v)), ty)
}
pub(super) fn boolean(b: bool, t: T) -> Expr {
    ex(ExprKind::Lit(Lit::Bool(b)), t.bool)
}
pub(super) fn s(v: &str, t: T) -> Expr {
    ex(ExprKind::Lit(Lit::Str(v.into())), t.str)
}
pub(super) fn un(op: UnOp, e: Expr) -> Expr {
    let ty = e.ty;
    ex(
        ExprKind::Unary {
            op,
            expr: Box::new(e),
        },
        ty,
    )
}
pub(super) fn not(e: Expr) -> Expr {
    un(UnOp::Not, e)
}
pub(super) fn neg(e: Expr) -> Expr {
    un(UnOp::Neg, e)
}
/// Arithmetic/bitwise binary op; result has the operand type.
pub(super) fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
    let ty = l.ty;
    ex(
        ExprKind::Binary {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
        },
        ty,
    )
}
pub(super) fn cmp(op: BinOp, l: Expr, r: Expr, t: T) -> Expr {
    ex(
        ExprKind::Binary {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
        },
        t.bool,
    )
}
pub(super) fn logic(op: LogicOp, l: Expr, r: Expr, t: T) -> Expr {
    ex(
        ExprKind::Logical {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
        },
        t.bool,
    )
}
pub(super) fn assign(place: Expr, value: Expr, t: T) -> Expr {
    ex(
        ExprKind::Assign {
            place: Box::new(place),
            value: Box::new(value),
        },
        t.unit,
    )
}
pub(super) fn cassign(op: BinOp, place: Expr, value: Expr, t: T) -> Expr {
    ex(
        ExprKind::CompoundAssign {
            op,
            place: Box::new(place),
            value: Box::new(value),
        },
        t.unit,
    )
}
pub(super) fn call(def: DefId, args: Vec<Expr>, ret: TyId) -> Expr {
    ex(
        ExprKind::Call {
            callee: Callee::Def(def, vec![]),
            args,
        },
        ret,
    )
}
pub(super) fn intr(i: Intrinsic, args: Vec<Expr>, ty: TyId) -> Expr {
    ex(
        ExprKind::Call {
            callee: Callee::Intrinsic(i),
            args,
        },
        ty,
    )
}
pub(super) fn print(args: Vec<Expr>, t: T) -> Expr {
    intr(Intrinsic::Print, args, t.unit)
}
pub(super) fn concat(a: Expr, b: Expr, t: T) -> Expr {
    intr(Intrinsic::StrConcat, vec![a, b], t.str)
}
pub(super) fn to_s(a: Expr, t: T) -> Expr {
    intr(Intrinsic::ToString, vec![a], t.str)
}
pub(super) fn cast(e: Expr, ty: TyId) -> Expr {
    ex(ExprKind::Cast(Box::new(e)), ty)
}
pub(super) fn ifx(c: Expr, a: Expr, b: Expr) -> Expr {
    let ty = a.ty;
    ex(
        ExprKind::If {
            cond: Box::new(c),
            then: Box::new(a),
            els: Box::new(b),
        },
        ty,
    )
}
pub(super) fn bexpr(stmts: Vec<Stmt>, value: Expr) -> Expr {
    let ty = value.ty;
    ex(
        ExprKind::Block(Block {
            stmts,
            value: Some(Box::new(value)),
            span: SP,
        }),
        ty,
    )
}

pub(super) fn st(kind: StmtKind) -> Stmt {
    Stmt { kind, span: SP }
}
pub(super) fn let_(local: LocalId, init: Expr) -> Stmt {
    st(StmtKind::Let {
        local,
        init: Some(init),
    })
}
pub(super) fn let_uninit(local: LocalId) -> Stmt {
    st(StmtKind::Let { local, init: None })
}
pub(super) fn se(e: Expr) -> Stmt {
    st(StmtKind::Expr(e))
}
pub(super) fn ret(e: Option<Expr>) -> Stmt {
    st(StmtKind::Return(e))
}
pub(super) fn if_(cond: Expr, then: Vec<Stmt>, els: Option<Vec<Stmt>>) -> Stmt {
    st(StmtKind::If {
        cond,
        then: block(then),
        els: els.map(block),
    })
}
pub(super) fn while_(label: Option<&str>, cond: Expr, body: Vec<Stmt>, step: Option<Expr>) -> Stmt {
    st(StmtKind::While {
        label: label.map(String::from),
        cond,
        body: block(body),
        step,
    })
}
pub(super) fn brk(label: Option<&str>) -> Stmt {
    st(StmtKind::Break(label.map(String::from)))
}
pub(super) fn cont(label: Option<&str>) -> Stmt {
    st(StmtKind::Continue(label.map(String::from)))
}
pub(super) fn sblock(stmts: Vec<Stmt>) -> Stmt {
    st(StmtKind::Block(block(stmts)))
}
pub(super) fn block(stmts: Vec<Stmt>) -> Block {
    Block {
        stmts,
        value: None,
        span: SP,
    }
}

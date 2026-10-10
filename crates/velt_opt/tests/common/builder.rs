//! Tiny VIR builder for hand-written test programs.

use velt_vir::vir::*;

/// Builds one `vir::Function` block by block.
pub struct FuncBuilder {
    f: Function,
}

impl FuncBuilder {
    /// New function; params occupy locals `0..params.len()`.
    pub fn new(symbol: &str, params: &[Ty], ret: Ty, linkage: Linkage) -> Self {
        let locals = params.iter().map(|&ty| LocalDecl::new(ty, None)).collect();
        FuncBuilder {
            f: Function {
                locals,
                ..Function::new(symbol.into(), params.to_vec(), ret, linkage)
            },
        }
    }
    /// New function with internal linkage.
    pub fn internal(symbol: &str, params: &[Ty], ret: Ty) -> Self {
        Self::new(symbol, params, ret, Linkage::Internal)
    }
    /// New exported function.
    pub fn export(symbol: &str, params: &[Ty], ret: Ty) -> Self {
        Self::new(symbol, params, ret, Linkage::Export)
    }
    /// Local holding parameter `i`.
    pub fn param(&self, i: u32) -> Local {
        assert!((i as usize) < self.f.params.len());
        Local(i)
    }
    /// Add a local of type `ty`.
    pub fn local(&mut self, ty: Ty) -> Local {
        self.f.locals.push(LocalDecl::new(ty, None));
        Local(self.f.locals.len() as u32 - 1)
    }
    /// Add a block (terminator defaults to `Unreachable`).
    pub fn block(&mut self) -> BlockId {
        self.f.blocks.push(BasicBlock {
            stmts: vec![],
            term: Terminator::Unreachable,
        });
        BlockId(self.f.blocks.len() as u32 - 1)
    }
    /// Append a statement to block `b`.
    pub fn push(&mut self, b: BlockId, s: Stmt) {
        self.f.blocks[b.0 as usize].stmts.push(s);
    }
    /// Append `l = rv` to block `b`.
    pub fn assign(&mut self, b: BlockId, l: Local, rv: Rvalue) {
        self.push(b, Stmt::Assign(Place::local(l), rv));
    }
    /// Set the terminator of block `b`.
    pub fn term(&mut self, b: BlockId, t: Terminator) {
        self.f.blocks[b.0 as usize].term = t;
    }
    /// `Goto(to)` from `b`.
    pub fn goto(&mut self, b: BlockId, to: BlockId) {
        self.term(b, Terminator::Goto(to));
    }
    /// Branch on a local.
    pub fn branch(&mut self, b: BlockId, cond: Local, then: BlockId, els: BlockId) {
        self.term(
            b,
            Terminator::Branch {
                cond: copy_local(cond),
                then,
                els,
            },
        );
    }
    /// Return `op` from `b`.
    pub fn ret(&mut self, b: BlockId, op: Operand) {
        self.term(b, Terminator::Return(op));
    }
    /// Emit a call terminator in `b` and return the continuation block.
    pub fn call(
        &mut self,
        b: BlockId,
        callee: Callee,
        args: Vec<Operand>,
        dest: Option<Local>,
    ) -> BlockId {
        let next = self.block();
        self.term(
            b,
            Terminator::Call {
                callee,
                args,
                dest: dest.map(Place::local),
                next,
            },
        );
        next
    }
    /// The finished function.
    pub fn finish(self) -> Function {
        self.f
    }
}

/// Builds a whole program.
pub struct ProgramBuilder {
    /// The program built so far.
    pub p: Program,
}

impl ProgramBuilder {
    /// Empty program with `STR_AGG`.
    pub fn new() -> Self {
        ProgramBuilder {
            p: Program {
                aggs: vec![AggLayout {
                    name: "string".into(),
                    size: 24,
                    align: 8,
                    fields: vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)],
                }],
                ..Default::default()
            },
        }
    }
    /// Declare an extern function.
    pub fn ext(&mut self, symbol: &str, params: &[Ty], ret: Ty, noreturn: bool) -> ExternId {
        self.p.externs.push(ExternFn {
            symbol: symbol.into(),
            params: params.to_vec(),
            ret,
            noreturn,
        });
        ExternId(self.p.externs.len() as u32 - 1)
    }
    /// Add an aggregate layout with fields `(ty, offset)`.
    pub fn agg(&mut self, name: &str, size: u32, align: u32, fields: &[(Ty, u32)]) -> AggId {
        self.p.aggs.push(AggLayout {
            name: name.into(),
            size,
            align,
            fields: fields.to_vec(),
        });
        AggId(self.p.aggs.len() as u32 - 1)
    }
    /// Add a read-only static.
    pub fn stat(&mut self, bytes: &[u8], align: u32) -> StaticId {
        self.stat_with(bytes, align, vec![])
    }
    /// Add a read-only static with address relocations (vtables).
    pub fn stat_with(&mut self, bytes: &[u8], align: u32, relocs: Vec<(u32, Const)>) -> StaticId {
        self.p.statics.push(StaticData {
            bytes: bytes.to_vec(),
            align,
            relocs,
        });
        StaticId(self.p.statics.len() as u32 - 1)
    }
    /// Reserve a function id (fill it later with `set`), for recursion.
    pub fn reserve(&mut self) -> FuncId {
        self.p
            .funcs
            .push(FuncBuilder::internal("__placeholder", &[], Ty::Unit).finish());
        FuncId(self.p.funcs.len() as u32 - 1)
    }
    /// Fill a function reserved with `reserve`.
    pub fn set(&mut self, id: FuncId, f: Function) {
        self.p.funcs[id.0 as usize] = f;
    }
    /// Add a function.
    pub fn add(&mut self, f: Function) -> FuncId {
        self.p.funcs.push(f);
        FuncId(self.p.funcs.len() as u32 - 1)
    }
    /// The finished program.
    pub fn finish(self) -> Program {
        self.p
    }
}

impl Default for ProgramBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Operand reading a whole local.
pub fn copy_local(l: Local) -> Operand {
    Operand::Copy(Place::local(l))
}
/// Operand reading a place.
pub fn copy_place(p: Place) -> Operand {
    Operand::Copy(p)
}
/// Integer (or Bool/Ptr) constant operand.
pub fn int(v: i128, ty: Ty) -> Operand {
    Operand::Const(Const::Int(v), ty)
}
/// Bool constant operand.
pub fn boolean(b: bool) -> Operand {
    Operand::Const(Const::Bool(b), Ty::Bool)
}
/// Float constant operand.
pub fn float(v: f64, ty: Ty) -> Operand {
    Operand::Const(Const::Float(v), ty)
}
/// `*l` viewed as `ty`.
pub fn deref(l: Local, ty: Ty) -> Place {
    Place {
        local: l,
        proj: vec![Proj::Deref(ty)],
    }
}
/// Field `n` of local `l`.
pub fn field(l: Local, n: u32) -> Place {
    Place {
        local: l,
        proj: vec![Proj::Field(n)],
    }
}
/// Binary rvalue.
pub fn bin(op: BinOp, a: Operand, b: Operand) -> Rvalue {
    Rvalue::Binary(op, a, b)
}
/// Every statement/terminator count, to assert on code shape.
pub fn count_calls(f: &Function) -> usize {
    f.blocks
        .iter()
        .filter(|b| matches!(b.term, Terminator::Call { .. }))
        .count()
}
/// Number of non-`Nop` statements.
pub fn count_stmts(f: &Function) -> usize {
    f.blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| !matches!(s, Stmt::Nop))
        .count()
}

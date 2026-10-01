//! Tests: hand-built VIR programs, executed via the JIT (same translation path) and emitted as
//! objects for all supported targets.

mod entry;
mod hot_swap;
mod jit;
mod jit_relocs;
mod link;
mod objects;

use velt_vir::vir::*;

// ───────────── tiny VIR builder ─────────────

/// Builds one `vir::Function` block by block.
pub(crate) struct FuncBuilder {
    f: Function,
}

impl FuncBuilder {
    /// New function; params occupy locals `0..params.len()`.
    pub fn new(symbol: &str, params: &[Ty], ret: Ty, linkage: Linkage) -> Self {
        let locals = params
            .iter()
            .map(|&ty| LocalDecl { ty, name: None })
            .collect();
        FuncBuilder {
            f: Function {
                symbol: symbol.into(),
                params: params.to_vec(),
                ret,
                locals,
                blocks: vec![],
                linkage,
                locs: vec![],
                param_attrs: vec![],
                is_poll: false,
            },
        }
    }
    /// New function with internal linkage.
    pub fn internal(symbol: &str, params: &[Ty], ret: Ty) -> Self {
        Self::new(symbol, params, ret, Linkage::Internal)
    }
    /// Local holding parameter `i`.
    pub fn param(&self, i: u32) -> Local {
        assert!((i as usize) < self.f.params.len());
        Local(i)
    }
    /// Add a local of type `ty`.
    pub fn local(&mut self, ty: Ty) -> Local {
        self.f.locals.push(LocalDecl { ty, name: None });
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
    /// Emit a call terminator in `b` and return the continuation block.
    pub fn call(
        &mut self,
        b: BlockId,
        callee: Callee,
        args: Vec<Operand>,
        dest: Option<Place>,
    ) -> BlockId {
        let next = self.block();
        self.term(
            b,
            Terminator::Call {
                callee,
                args,
                dest,
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

/// Operand reading a whole local.
pub(crate) fn copy_local(l: Local) -> Operand {
    Operand::Copy(Place::local(l))
}
/// Operand reading a place.
pub(crate) fn copy_place(p: Place) -> Operand {
    Operand::Copy(p)
}
/// Integer (or Bool/Ptr) constant operand.
pub(crate) fn int(v: i128, ty: Ty) -> Operand {
    Operand::Const(Const::Int(v), ty)
}
/// Float constant operand.
pub(crate) fn float(v: f64, ty: Ty) -> Operand {
    Operand::Const(Const::Float(v), ty)
}
/// Place with projections.
pub(crate) fn place(l: Local, proj: Vec<Proj>) -> Place {
    Place { local: l, proj }
}

/// `STR_AGG` layout (rt `VeltStr`).
pub(crate) fn str_agg() -> AggLayout {
    AggLayout {
        name: "string".into(),
        size: 24,
        align: 8,
        fields: vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)],
    }
}

/// A program builder with the M1 runtime externs declared exactly per rt_abi.md.
pub(crate) struct ProgramBuilder {
    /// The program built so far.
    pub p: Program,
}

/// Extern ids of the M1 runtime functions: each field is the id of `velt_rt_<field>`.
pub(crate) struct Rt {
    pub write_str: ExternId,
    pub write_i64: ExternId,
    pub write_u64: ExternId,
    pub write_f64: ExternId,
    pub write_bool: ExternId,
    pub write_byte: ExternId,
    pub flush: ExternId,
    pub panic: ExternId,
    pub exit: ExternId,
    pub alloc: ExternId,
    pub free: ExternId,
    pub str_from_i64: ExternId,
    pub str_concat: ExternId,
    pub str_drop: ExternId,
    pub str_cmp: ExternId,
    pub pow_i64: ExternId,
}

impl ProgramBuilder {
    /// Empty program (with `STR_AGG`) and all M1 rt externs.
    pub fn new() -> (Self, Rt) {
        let mut pb = ProgramBuilder {
            p: Program {
                aggs: vec![str_agg()],
                funcs: vec![],
                externs: vec![],
                statics: vec![],
                files: vec![],
            },
        };
        use Ty::*;
        let rt = Rt {
            write_str: pb.ext("velt_rt_write_str", &[U32, Ptr], Unit, false),
            write_i64: pb.ext("velt_rt_write_i64", &[U32, I64], Unit, false),
            write_u64: pb.ext("velt_rt_write_u64", &[U32, U64], Unit, false),
            write_f64: pb.ext("velt_rt_write_f64", &[U32, F64], Unit, false),
            write_bool: pb.ext("velt_rt_write_bool", &[U32, Bool], Unit, false),
            write_byte: pb.ext("velt_rt_write_byte", &[U32, U8], Unit, false),
            flush: pb.ext("velt_rt_flush", &[], Unit, false),
            panic: pb.ext("velt_rt_panic", &[Ptr], Unit, true),
            exit: pb.ext("velt_rt_exit", &[I32], Unit, true),
            alloc: pb.ext("velt_rt_alloc", &[U64, U64], Ptr, false),
            free: pb.ext("velt_rt_free", &[Ptr, U64, U64], Unit, false),
            str_from_i64: pb.ext("velt_rt_str_from_i64", &[I64, Ptr], Unit, false),
            str_concat: pb.ext("velt_rt_str_concat", &[Ptr, Ptr, Ptr], Unit, false),
            str_drop: pb.ext("velt_rt_str_drop", &[Ptr], Unit, false),
            str_cmp: pb.ext("velt_rt_str_cmp", &[Ptr, Ptr], I32, false),
            pow_i64: pb.ext("velt_rt_pow_i64", &[I64, I64], I64, false),
        };
        (pb, rt)
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
    /// Add an aggregate layout.
    pub fn agg(&mut self, a: AggLayout) -> AggId {
        self.p.aggs.push(a);
        AggId(self.p.aggs.len() as u32 - 1)
    }
    /// Add a read-only static.
    pub fn stat(&mut self, bytes: &[u8]) -> StaticId {
        self.stat_with(bytes, 1, vec![])
    }
    /// Add a read-only static with address relocations.
    pub fn stat_with(&mut self, bytes: &[u8], align: u32, relocs: Vec<(u32, Const)>) -> StaticId {
        self.p.statics.push(StaticData {
            bytes: bytes.to_vec(),
            align,
            relocs,
        });
        StaticId(self.p.statics.len() as u32 - 1)
    }
    /// Reserve a function id (fill it later with `set`).
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
}

/// Helpers for emitting console output inside a function under construction.
pub(crate) struct Out<'a> {
    /// Function being built.
    pub fb: &'a mut FuncBuilder,
    /// Runtime extern ids.
    pub rt: &'a Rt,
    /// Block where the next call is emitted.
    pub cur: BlockId,
}

impl Out<'_> {
    /// `write_i64(1, v)`.
    pub fn i64(&mut self, v: Operand) {
        self.cur = self.fb.call(
            self.cur,
            Callee::Extern(self.rt.write_i64),
            vec![int(1, Ty::U32), v],
            None,
        );
    }
    /// `write_u64(1, v)`.
    pub fn u64(&mut self, v: Operand) {
        self.cur = self.fb.call(
            self.cur,
            Callee::Extern(self.rt.write_u64),
            vec![int(1, Ty::U32), v],
            None,
        );
    }
    /// `write_f64(1, v)`.
    pub fn f64(&mut self, v: Operand) {
        self.cur = self.fb.call(
            self.cur,
            Callee::Extern(self.rt.write_f64),
            vec![int(1, Ty::U32), v],
            None,
        );
    }
    /// `write_bool(1, v)`.
    pub fn bool(&mut self, v: Operand) {
        self.cur = self.fb.call(
            self.cur,
            Callee::Extern(self.rt.write_bool),
            vec![int(1, Ty::U32), v],
            None,
        );
    }
    /// `write_byte(1, b)`.
    pub fn byte(&mut self, b: u8) {
        self.cur = self.fb.call(
            self.cur,
            Callee::Extern(self.rt.write_byte),
            vec![int(1, Ty::U32), int(b as i128, Ty::U8)],
            None,
        );
    }
    /// Newline.
    pub fn nl(&mut self) {
        self.byte(b'\n');
    }
    /// Print any scalar value (ints widened by signedness, f32 promoted), then a newline.
    pub fn line(&mut self, v: Operand, ty: Ty) {
        match ty {
            Ty::Bool => self.bool(v),
            Ty::F64 => self.f64(v),
            Ty::F32 => {
                let t = self.fb.local(Ty::F64);
                self.fb.assign(self.cur, t, Rvalue::Cast(v, Ty::F64));
                self.f64(copy_local(t));
            }
            t if t.is_signed() => {
                let w = self.fb.local(Ty::I64);
                self.fb.assign(self.cur, w, Rvalue::Cast(v, Ty::I64));
                self.i64(copy_local(w));
            }
            _ => {
                let w = self.fb.local(Ty::U64);
                self.fb.assign(self.cur, w, Rvalue::Cast(v, Ty::U64));
                self.u64(copy_local(w));
            }
        }
        self.nl();
    }
}

/// Finish `velt_main` returning `code`.
pub(crate) fn finish_main(mut fb: FuncBuilder, cur: BlockId, code: i32) -> Function {
    fb.term(cur, Terminator::Return(int(code as i128, Ty::I32)));
    fb.finish()
}

/// Exported `velt_main() -> I32` with its entry block.
pub(crate) fn main_fb() -> (FuncBuilder, BlockId) {
    let mut fb = FuncBuilder::new("velt_main", &[], Ty::I32, Linkage::Export);
    let b = fb.block();
    (fb, b)
}

pub(crate) mod programs;

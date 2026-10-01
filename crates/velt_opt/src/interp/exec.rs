//! Execution: frames (one stack slot per local), place resolution, statements, terminators
//! and calls (direct, extern via the host, indirect via address tokens).

use velt_vir::vir::{
    BlockId, Callee, Const, ExternId, FuncId, Function, Operand, Place, Proj, Rvalue, Stmt,
    Terminator, Ty,
};

use super::{scalar, Host, Interp, Trap};

/// Opaque address tokens for functions and externs (outside every memory region).
const FUNC_TOKENS: u64 = 0xF000_0000_0000;
const EXTERN_TOKENS: u64 = 0xE000_0000_0000;
const TOKEN_STRIDE: u64 = 16;

/// A computed value: raw scalar bits, or the bytes of an aggregate.
enum Val {
    Scalar(u64),
    Bytes(Vec<u8>),
}

/// Locals of one activation: the address of each local's slot.
struct Frame<'p> {
    func: &'p Function,
    slots: Vec<u64>,
}

impl<H: Host> Interp<'_, H> {
    pub(super) fn call_func(&mut self, id: FuncId, args: &[u64]) -> Result<u64, Trap> {
        let program = self.program;
        let func = program
            .funcs
            .get(id.0 as usize)
            .ok_or_else(|| Trap::Invalid(format!("unknown function #{}", id.0)))?;
        if args.len() != func.params.len() || func.locals.len() < args.len() {
            return Err(Trap::Invalid(format!(
                "arity mismatch calling `{}`",
                func.symbol
            )));
        }
        if self.depth >= super::MAX_DEPTH {
            return Err(Trap::StackOverflow);
        }
        let mark = self.mem.stack_mark();
        self.depth += 1;
        let result = self.activate(func, args);
        self.depth -= 1;
        self.mem.stack_release(mark);
        result
    }

    fn activate(&mut self, func: &Function, args: &[u64]) -> Result<u64, Trap> {
        let mut slots = Vec::with_capacity(func.locals.len());
        for local in &func.locals {
            let (size, align) = self.size_align(local.ty)?;
            slots.push(self.mem.alloc_stack(size, align)?);
        }
        for (i, &bits) in args.iter().enumerate() {
            self.mem.write_scalar(slots[i], func.params[i], bits)?;
        }
        self.run(&Frame { func, slots })
    }

    fn size_align(&self, ty: Ty) -> Result<(u32, u32), Trap> {
        match ty {
            Ty::Agg(id) => self
                .program
                .aggs
                .get(id.0 as usize)
                .map(|a| (a.size, a.align))
                .ok_or_else(|| Trap::Invalid(format!("unknown aggregate #{}", id.0))),
            _ => Ok(self.program.size_align(ty)),
        }
    }

    fn tick(&mut self) -> Result<(), Trap> {
        self.fuel = self.fuel.checked_sub(1).ok_or(Trap::OutOfFuel)?;
        Ok(())
    }

    fn run(&mut self, frame: &Frame) -> Result<u64, Trap> {
        let mut bb = BlockId(0);
        loop {
            let block = frame
                .func
                .blocks
                .get(bb.0 as usize)
                .ok_or_else(|| Trap::Invalid(format!("unknown block bb{}", bb.0)))?;
            for s in &block.stmts {
                self.tick()?;
                self.stmt(frame, s)?;
            }
            self.tick()?;
            match self.term(frame, &block.term)? {
                Flow::Jump(next) => bb = next,
                Flow::Return(bits) => return Ok(bits),
            }
        }
    }

    /// Address and type of a place.
    fn place(&self, frame: &Frame, p: &Place) -> Result<(u64, Ty), Trap> {
        let i = p.local.0 as usize;
        let (mut addr, mut ty) = match (frame.slots.get(i), frame.func.locals.get(i)) {
            (Some(&a), Some(l)) => (a, l.ty),
            _ => return Err(Trap::Invalid(format!("unknown local _{i}"))),
        };
        for proj in &p.proj {
            match (proj, ty) {
                (Proj::Field(n), Ty::Agg(id)) => {
                    let field = self
                        .program
                        .aggs
                        .get(id.0 as usize)
                        .and_then(|a| a.fields.get(*n as usize))
                        .ok_or_else(|| Trap::Invalid(format!("bad field {n} of agg #{}", id.0)))?;
                    addr += u64::from(field.1);
                    ty = field.0;
                }
                (Proj::Deref(pointee), Ty::Ptr) => {
                    addr = self.mem.read_scalar(addr, Ty::Ptr)?;
                    ty = *pointee;
                }
                (Proj::Cast(id), _) => ty = Ty::Agg(*id),
                _ => return Err(Trap::Invalid(format!("projection {proj:?} on {ty:?}"))),
            }
        }
        Ok((addr, ty))
    }

    fn read(&self, addr: u64, ty: Ty) -> Result<Val, Trap> {
        match ty {
            Ty::Agg(_) => {
                let (size, _) = self.size_align(ty)?;
                Ok(Val::Bytes(self.mem.read(addr, u64::from(size))?.to_vec()))
            }
            Ty::Unit => Ok(Val::Scalar(0)),
            _ => Ok(Val::Scalar(self.mem.read_scalar(addr, ty)?)),
        }
    }

    fn write(&mut self, addr: u64, ty: Ty, val: Val) -> Result<(), Trap> {
        match (ty, val) {
            (Ty::Unit, _) => Ok(()),
            (Ty::Agg(_), Val::Bytes(b)) => self.mem.write(addr, &b),
            (_, Val::Scalar(bits)) => self.mem.write_scalar(addr, ty, bits),
            _ => Err(Trap::Invalid(format!(
                "value/place kind mismatch for {ty:?}"
            ))),
        }
    }

    fn operand(&self, frame: &Frame, op: &Operand) -> Result<(Val, Ty), Trap> {
        match op {
            Operand::Copy(p) => {
                let (addr, ty) = self.place(frame, p)?;
                Ok((self.read(addr, ty)?, ty))
            }
            Operand::Const(c, ty) => Ok((Val::Scalar(self.constant(c, *ty)?), *ty)),
        }
    }

    fn scalar(&self, frame: &Frame, op: &Operand) -> Result<(u64, Ty), Trap> {
        match self.operand(frame, op)? {
            (Val::Scalar(bits), ty) => Ok((bits, ty)),
            (Val::Bytes(_), ty) => Err(Trap::Invalid(format!("aggregate {ty:?} used as scalar"))),
        }
    }

    fn constant(&self, c: &Const, ty: Ty) -> Result<u64, Trap> {
        Ok(match c {
            Const::Int(v) => scalar::int_const(*v, ty),
            Const::Float(x) => scalar::float_const(*x, ty),
            Const::Bool(b) => u64::from(*b),
            Const::Unit => 0,
            Const::Static(id) => *self
                .statics
                .get(id.0 as usize)
                .ok_or_else(|| Trap::Invalid(format!("unknown static #{}", id.0)))?,
            Const::Func(id) => FUNC_TOKENS + u64::from(id.0) * TOKEN_STRIDE,
            Const::Extern(id) => EXTERN_TOKENS + u64::from(id.0) * TOKEN_STRIDE,
        })
    }

    /// Write the address of every static relocation target into its slot.
    pub(super) fn patch_relocs(&mut self) {
        let program = self.program;
        for (s, &base) in program.statics.iter().zip(&self.statics) {
            for (offset, target) in &s.relocs {
                let addr = match target {
                    Const::Static(_) | Const::Func(_) | Const::Extern(_) => {
                        self.constant(target, Ty::Ptr).unwrap_or(0)
                    }
                    _ => 0,
                };
                // An out-of-bounds slot is malformed VIR; there is nothing to patch.
                let _ = self
                    .mem
                    .patch_static(base + u64::from(*offset), &addr.to_le_bytes());
            }
        }
    }

    fn stmt(&mut self, frame: &Frame, s: &Stmt) -> Result<(), Trap> {
        match s {
            Stmt::Nop => Ok(()),
            Stmt::MemCopy { dst, src, size } => {
                let (dst, _) = self.scalar(frame, dst)?;
                let (src, _) = self.scalar(frame, src)?;
                let bytes = self.mem.read(src, *size)?.to_vec();
                self.mem.write(dst, &bytes)
            }
            Stmt::MemCopyDyn {
                dst,
                src,
                len,
                overlapping,
            } => {
                let (dst, _) = self.scalar(frame, dst)?;
                let (src, _) = self.scalar(frame, src)?;
                let (len, _) = self.scalar(frame, len)?;
                // memcpy of overlapping regions is undefined behaviour on real targets.
                if !overlapping
                    && len > 0
                    && dst < src.saturating_add(len)
                    && src < dst.saturating_add(len)
                {
                    return Err(Trap::Invalid("memcopy regions overlap".into()));
                }
                let bytes = self.mem.read(src, len)?.to_vec();
                self.mem.write(dst, &bytes)
            }
            Stmt::MemSet { dst, byte, len } => {
                let (dst, _) = self.scalar(frame, dst)?;
                let (byte, _) = self.scalar(frame, byte)?;
                let (len, _) = self.scalar(frame, len)?;
                // Bounds-check before allocating so a wild length traps instead of exhausting RAM.
                self.mem.read(dst, len)?;
                self.mem.write(dst, &vec![byte as u8; len as usize])
            }
            Stmt::Assign(place, rv) => {
                let val = self.rvalue(frame, rv)?;
                let (addr, ty) = self.place(frame, place)?;
                self.write(addr, ty, val)
            }
        }
    }

    fn rvalue(&mut self, frame: &Frame, rv: &Rvalue) -> Result<Val, Trap> {
        Ok(Val::Scalar(match rv {
            Rvalue::Use(op) => return Ok(self.operand(frame, op)?.0),
            Rvalue::Unary(op, a) => {
                let (bits, ty) = self.scalar(frame, a)?;
                scalar::unary(*op, ty, bits)?
            }
            Rvalue::Binary(op, a, b) => {
                let (x, tx) = self.scalar(frame, a)?;
                let (y, ty) = self.scalar(frame, b)?;
                scalar::binary(*op, tx, x, ty, y)?
            }
            Rvalue::Cast(a, to) => {
                let (bits, from) = self.scalar(frame, a)?;
                scalar::cast(from, *to, bits)?
            }
            Rvalue::AddrOf(p) => self.place(frame, p)?.0,
            Rvalue::Aggregate(id, ops) => return self.aggregate(frame, *id, ops),
        }))
    }

    fn aggregate(
        &mut self,
        frame: &Frame,
        id: velt_vir::vir::AggId,
        ops: &[Operand],
    ) -> Result<Val, Trap> {
        let layout = self
            .program
            .aggs
            .get(id.0 as usize)
            .ok_or_else(|| Trap::Invalid(format!("unknown aggregate #{}", id.0)))?;
        let mut bytes = vec![0u8; layout.size as usize];
        for (op, &(fty, offset)) in ops.iter().zip(&layout.fields) {
            let chunk = match self.operand(frame, op)?.0 {
                Val::Bytes(b) => b,
                Val::Scalar(bits) => {
                    let size = fty.scalar_size().unwrap_or(0) as usize;
                    bits.to_le_bytes()[..size].to_vec()
                }
            };
            let end = offset as usize + chunk.len();
            bytes
                .get_mut(offset as usize..end)
                .ok_or_else(|| Trap::Invalid(format!("field outside aggregate #{}", id.0)))?
                .copy_from_slice(&chunk);
        }
        Ok(Val::Bytes(bytes))
    }
}

/// Where control goes after a terminator.
enum Flow {
    Jump(BlockId),
    Return(u64),
}

impl<H: Host> Interp<'_, H> {
    fn term(&mut self, frame: &Frame, t: &Terminator) -> Result<Flow, Trap> {
        Ok(match t {
            Terminator::Goto(b) => Flow::Jump(*b),
            Terminator::Branch { cond, then, els } => {
                let (c, _) = self.scalar(frame, cond)?;
                Flow::Jump(if c != 0 { *then } else { *els })
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let (v, ty) = self.scalar(frame, value)?;
                let hit = cases
                    .iter()
                    .find(|(k, _)| scalar::encode_int(*k, ty) == v)
                    .map_or(*default, |(_, b)| *b);
                Flow::Jump(hit)
            }
            Terminator::Return(op) => match frame.func.ret {
                Ty::Unit => Flow::Return(0),
                _ => Flow::Return(self.scalar(frame, op)?.0),
            },
            Terminator::Call {
                callee,
                args,
                dest,
                next,
            } => {
                let mut values = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.scalar(frame, a)?.0);
                }
                let (result, ret) = self.dispatch(frame, callee, &values)?;
                if let (Some(d), false) = (dest, ret == Ty::Unit) {
                    let (addr, ty) = self.place(frame, d)?;
                    self.write(addr, ty, Val::Scalar(result))?;
                }
                Flow::Jump(*next)
            }
            Terminator::Unreachable => return Err(Trap::Unreachable),
        })
    }

    /// Perform a call; returns (result bits, callee return type).
    fn dispatch(
        &mut self,
        frame: &Frame,
        callee: &Callee,
        args: &[u64],
    ) -> Result<(u64, Ty), Trap> {
        match callee {
            Callee::Func(id) => {
                let ret = self
                    .program
                    .funcs
                    .get(id.0 as usize)
                    .map_or(Ty::Unit, |f| f.ret);
                Ok((self.call_func(*id, args)?, ret))
            }
            Callee::Extern(id) => self.call_extern(*id, args),
            Callee::Ptr {
                target,
                params,
                ret,
            } => {
                let (addr, _) = self.scalar(frame, target)?;
                let program = self.program;
                let matches = |p: &[Ty], r: Ty| p == params.as_slice() && r == *ret;
                if let Some(i) = token_index(addr, FUNC_TOKENS) {
                    if let Some(f) = program
                        .funcs
                        .get(i as usize)
                        .filter(|f| matches(&f.params, f.ret))
                    {
                        return Ok((self.call_func(FuncId(i), args)?, f.ret));
                    }
                }
                if let Some(i) = token_index(addr, EXTERN_TOKENS) {
                    if program
                        .externs
                        .get(i as usize)
                        .is_some_and(|e| matches(&e.params, e.ret))
                    {
                        return self.call_extern(ExternId(i), args);
                    }
                }
                Err(Trap::BadCall(addr))
            }
        }
    }

    fn call_extern(&mut self, id: ExternId, args: &[u64]) -> Result<(u64, Ty), Trap> {
        let program = self.program;
        let ext = program
            .externs
            .get(id.0 as usize)
            .ok_or_else(|| Trap::Invalid(format!("unknown extern #{}", id.0)))?;
        let bits = self.host.call(ext, args, &mut self.mem)?;
        Ok((bits, ext.ret))
    }
}

/// Index encoded in an address token relative to `base`, if `addr` is one.
fn token_index(addr: u64, base: u64) -> Option<u32> {
    let off = addr.checked_sub(base)?;
    (off % TOKEN_STRIDE == 0)
        .then_some(off / TOKEN_STRIDE)?
        .try_into()
        .ok()
}

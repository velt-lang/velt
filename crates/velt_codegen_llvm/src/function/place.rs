//! Places: resolving locals + projections to memory locations, reading/writing them, and
//! aggregate copies.

use velt_vir::vir::{AggId, Place, Proj, Ty};

use super::{Emitter, Loc, Val};
use crate::types::{aggregate, offset_align, size_align};
use crate::CodegenResult;

impl Emitter<'_> {
    pub(super) fn place(&mut self, p: &Place) -> CodegenResult<Loc> {
        let index = p.local.0 as usize;
        let Some(decl) = self.function.locals.get(index) else {
            bail!("unknown local _{index}")
        };
        let mut loc = match decl.ty {
            Ty::Unit => Loc::Unit,
            ty => Loc::Mem {
                base: format!("%l{index}"),
                offset: 0,
                base_align: size_align(self.program, ty)?.1,
                ty,
                root: Some(p.local.0),
            },
        };
        for proj in &p.proj {
            loc = self.project(loc, proj)?;
        }
        Ok(loc)
    }

    fn project(&mut self, loc: Loc, proj: &Proj) -> CodegenResult<Loc> {
        Ok(match (proj, loc) {
            (
                Proj::Field(n),
                Loc::Mem {
                    base,
                    offset,
                    base_align,
                    ty: Ty::Agg(id),
                    root,
                },
            ) => {
                let (field_ty, field_offset) = self.field(id, *n)?;
                Loc::Mem {
                    base,
                    offset: offset + u64::from(field_offset),
                    base_align,
                    ty: field_ty,
                    root,
                }
            }
            (Proj::Deref(pointee), l) if l.ty() == Ty::Ptr => {
                let Val::Scalar(ptr) = self.read(l)? else {
                    unreachable!("ICE: a Ptr place always reads as a scalar")
                };
                // Pointers produced by lowering point at properly aligned values of the
                // pointee's layout.
                let (_, align) = size_align(self.program, *pointee)?;
                Loc::Mem {
                    base: ptr,
                    offset: 0,
                    base_align: align,
                    ty: *pointee,
                    root: None,
                }
            }
            (
                Proj::Cast(id),
                Loc::Mem {
                    base,
                    offset,
                    base_align,
                    root,
                    ..
                },
            ) => {
                aggregate(self.program, *id)?;
                Loc::Mem {
                    base,
                    offset,
                    base_align,
                    ty: Ty::Agg(*id),
                    root,
                }
            }
            (proj, l) => bail!(
                "invalid projection {proj:?} on a place of type {:?}",
                l.ty()
            ),
        })
    }

    fn field(&self, id: AggId, n: u32) -> CodegenResult<(Ty, u32)> {
        let layout = aggregate(self.program, id)?;
        match layout.fields.get(n as usize) {
            Some(&f) => Ok(f),
            None => bail!("aggregate `{}` has no field {n}", layout.name),
        }
    }

    /// Pointer to a memory location and its known alignment.
    pub(super) fn address(&mut self, loc: &Loc) -> CodegenResult<(String, u32)> {
        match loc {
            Loc::Mem {
                base,
                offset: 0,
                base_align,
                ..
            } => Ok((base.clone(), *base_align)),
            Loc::Mem {
                base,
                offset,
                base_align,
                ..
            } => {
                // Field offsets stay inside the aggregate object, hence `inbounds`.
                let ptr = self.inst(format!(
                    "getelementptr inbounds i8, ptr {base}, i64 {offset}"
                ));
                Ok((ptr, offset_align(*base_align, *offset)))
            }
            Loc::Unit => bail!("a unit place has no address"),
        }
    }

    pub(super) fn read(&mut self, loc: Loc) -> CodegenResult<Val> {
        Ok(match loc.ty() {
            Ty::Unit => Val::Unit,
            Ty::Agg(_) => Val::Agg(loc),
            scalar => {
                let (ptr, align) = self.address(&loc)?;
                let t = self.memory_type(scalar);
                let v = self.inst(format!("load {t}, ptr {ptr}, align {align}"));
                Val::Scalar(self.narrow_after_load(v, scalar))
            }
        })
    }

    pub(super) fn write(&mut self, loc: Loc, val: Val) -> CodegenResult<()> {
        match (&loc, val) {
            (Loc::Unit, _) | (_, Val::Unit) => {}
            (Loc::Mem { ty: Ty::Agg(_), .. }, Val::Agg(src)) => self.copy_aggregate(&loc, &src)?,
            (Loc::Mem { ty, .. }, Val::Scalar(x)) if ty.is_scalar() => {
                let t = self.memory_type(*ty);
                let x = self.widen_for_store(&x, *ty);
                let (ptr, align) = self.address(&loc)?;
                self.line(format!("store {t} {x}, ptr {ptr}, align {align}"));
            }
            (l, _) => bail!("value/place kind mismatch for a place of type {:?}", l.ty()),
        }
        Ok(())
    }

    /// Whole-aggregate assignment. The source may overlap the destination (e.g. `x = x`, or
    /// both reached through pointers), so `memmove` unless both are distinct locals.
    fn copy_aggregate(&mut self, dst: &Loc, src: &Loc) -> CodegenResult<()> {
        let (size, _) = size_align(self.program, dst.ty())?;
        if size == 0 {
            return Ok(());
        }
        let disjoint = matches!(
            (dst, src),
            (Loc::Mem { root: Some(a), .. }, Loc::Mem { root: Some(b), .. }) if a != b
        );
        let (dst_ptr, dst_align) = self.address(dst)?;
        let (src_ptr, src_align) = self.address(src)?;
        let kind = if disjoint { "memcpy" } else { "memmove" };
        self.mem_intrinsic(
            kind,
            (&dst_ptr, dst_align),
            (&src_ptr, src_align),
            &size.to_string(),
        );
        Ok(())
    }

    /// `llvm.memcpy` / `llvm.memmove` of `size` bytes (a constant or an `i64` SSA value).
    pub(super) fn mem_intrinsic(
        &mut self,
        kind: &str,
        (dst, dst_align): (&str, u32),
        (src, src_align): (&str, u32),
        size: &str,
    ) {
        let name = format!("@llvm.{kind}.p0.p0.i64");
        self.intrinsics
            .need(format!("declare void {name}(ptr, ptr, i64, i1)"));
        self.line(format!(
            "call void {name}(ptr align {dst_align} {dst}, ptr align {src_align} {src}, i64 {size}, i1 false)"
        ));
    }
}

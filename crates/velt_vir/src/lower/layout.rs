//! Aggregate layouts. Fields keep declaration order at their natural alignment (offset rounded up
//! to the field's alignment, size rounded up to the aggregate's alignment, minimum size 1).
//!
//! - `void` fields (`IoResult<void>`) are zero-sized: they have no VIR field, so HIR field `i`
//!   is VIR field [`vir_field`](Cx::vir_field)`(t, i)` (the non-`void` fields before it).
//! - Tagged enums: a base aggregate `{ tag: u32 }` sized for the largest variant, plus one "view"
//!   aggregate per variant `{ tag: u32, payload... }` reached with `Proj::Cast`. The tag is the
//!   variant index. `Result<T, E>` works the same with a `u8` tag (views: Ok, Err; a `void`
//!   payload has no field).
//! - `T | null` without a niche: `{ some: bool, value: T }`.
//! - Class objects: `[vtable: ptr]` + all fields (base-class fields first, so a subclass object
//!   starts with its base's layout and upcasts are free). The vtable pointer exists when the
//!   class hierarchy has more than one class or any virtual slot; it points to a dispatcher
//!   function (see glue/vtable.rs).
//! - Shared boxes: `{ count: u64, value: T }`; closure environments: `{ drop: ptr, clone: ptr,
//!   transfer: ptr, captures... }` (borrowed captures are pointers; closure.rs).

use std::collections::{HashMap, HashSet};

use velt_sema::hir::{self, AdtKind, DefId, PassMode, TyId, TyKind};

use super::{ice, Cx};
use crate::vir::{AggId, AggLayout, Ty};

#[derive(Default)]
pub(super) struct Layouts {
    value: HashMap<TyId, AggId>,
    views: HashMap<TyId, Vec<AggId>>,
    objs: HashMap<TyId, AggId>,
    /// Roots of the class hierarchies whose objects start with a vtable pointer (built once).
    headers: Option<HashSet<DefId>>,
    boxes: HashMap<TyId, AggId>,
    envs: HashMap<(DefId, Vec<TyId>), AggId>,
    in_progress: HashSet<TyId>,
    array: Option<AggId>,
    closure: Option<AggId>,
    dyn_value: Option<AggId>,
    /// Tables of generator instances (async_fn/gen_object.rs).
    pub(super) gen_tables: HashMap<(DefId, Vec<TyId>), crate::vir::StaticId>,
    /// State of the `Promise.all` wrapper future (async_fn/tasks.rs).
    pub(super) all_wrap: Option<AggId>,
    /// State of the settling `Promise.all` wrapper per child result type (all_settle.rs).
    pub(super) settle_wraps: HashMap<TyId, AggId>,
    /// State of the wrapper boxing a kept `Promise.race` per result slot type (kept.rs).
    pub(super) race_boxes: HashMap<TyId, AggId>,
    /// State of the wrapper widening a promise, per target promise type (async_fn/widen.rs).
    pub(super) widen_boxes: HashMap<TyId, AggId>,
    /// Failure context of JSON decoders (json/).
    pub(super) json_ctx: Option<AggId>,
    /// Promise-value wrappers of throwing async functions: layout and inner-state field.
    pub(super) value_wraps: HashMap<(DefId, Vec<TyId>), (AggId, u32)>,
    /// Vtable statics (glue/vtable.rs).
    pub(super) vtables: HashMap<super::VtableKey, crate::vir::StaticId>,
    pub(super) drop_memo: HashMap<TyId, bool>,
    /// Foreign (runtime-facing) layouts of types containing boxed values (foreign.rs).
    pub(super) foreign: HashMap<TyId, Ty>,
}

impl Cx<'_> {
    pub(super) fn size_align(&self, t: Ty) -> (u32, u32) {
        match t {
            Ty::Agg(a) => {
                let l = &self.aggs[a.0 as usize];
                (l.size, l.align)
            }
            Ty::Unit => (0, 1),
            s => {
                let n = s.scalar_size().unwrap_or_else(|| ice("scalar size"));
                (n, n)
            }
        }
    }

    /// Lay out `tys` in order at natural alignment.
    pub(super) fn new_agg(&mut self, name: String, tys: &[Ty]) -> AggId {
        let mut off = 0u32;
        let mut align = 1u32;
        let mut fields = vec![];
        for &t in tys {
            if t == Ty::Unit {
                ice(format_args!("zero-sized field in aggregate {name}"));
            }
            let (s, a) = self.size_align(t);
            off = off.next_multiple_of(a);
            fields.push((t, off));
            off += s;
            align = align.max(a);
        }
        let size = off.max(1).next_multiple_of(align);
        self.push_agg(AggLayout {
            name,
            size,
            align,
            fields,
        })
    }

    /// Register a finished layout (async state structs compute their own, see async_fn/).
    pub(super) fn push_agg(&mut self, l: AggLayout) -> AggId {
        self.aggs.push(l);
        AggId(self.aggs.len() as u32 - 1)
    }

    pub(super) fn array_agg(&mut self) -> AggId {
        if let Some(a) = self.lay.array {
            return a;
        }
        let a = self.new_agg("array".into(), &[Ty::Ptr, Ty::U64, Ty::U64]);
        self.lay.array = Some(a);
        a
    }

    pub(super) fn closure_agg(&mut self) -> AggId {
        if let Some(a) = self.lay.closure {
            return a;
        }
        let a = self.new_agg("closure".into(), &[Ty::Ptr, Ty::Ptr]);
        self.lay.closure = Some(a);
        a
    }

    pub(super) fn dyn_agg(&mut self) -> AggId {
        if let Some(a) = self.lay.dyn_value {
            return a;
        }
        let a = self.new_agg("dyn".into(), &[Ty::Ptr, Ty::Ptr]);
        self.lay.dyn_value = Some(a);
        a
    }

    /// Aggregate of a by-value struct/anon/tuple/option/enum/result type.
    pub(super) fn value_agg(&mut self, t: TyId) -> AggId {
        if let Some(&a) = self.lay.value.get(&t) {
            return a;
        }
        if !self.lay.in_progress.insert(t) {
            ice(format_args!(
                "type `{}` contains itself without indirection",
                self.type_name(t)
            ));
        }
        let name = self.type_name(t);
        let a = match self.kind(t) {
            TyKind::Adt(d, _) if matches!(self.hir.def(d), hir::Def::Enum(_)) => {
                self.tagged_base(t, Ty::U32)
            }
            TyKind::Result(..) => self.tagged_base(t, Ty::U8),
            TyKind::Adt(..) => {
                let tys = self.adt_field_tys(t);
                let tys = self.stored_tys(tys);
                self.new_agg(name, &tys)
            }
            TyKind::Tuple(es) => {
                let tys = self.stored_tys(es);
                self.new_agg("tuple".into(), &tys)
            }
            TyKind::Option(e) => {
                let vt = self.ty(e);
                self.new_agg("option".into(), &[Ty::Bool, vt])
            }
            k => ice(format_args!("no value aggregate for {k:?}")),
        };
        self.lay.in_progress.remove(&t);
        self.lay.value.insert(t, a);
        a
    }

    /// VIR types of the fields that occupy memory (`void` fields have none).
    fn stored_tys(&mut self, fields: Vec<TyId>) -> Vec<Ty> {
        fields
            .into_iter()
            .map(|f| self.ty(f))
            .filter(|&t| t != Ty::Unit)
            .collect()
    }

    /// VIR field index of HIR field `i` of a struct/tuple value or class object `t` (after the
    /// vtable pointer, skipping `void` fields). Meaningless for a `void` field itself.
    pub(super) fn vir_field(&mut self, t: TyId, i: u32) -> u32 {
        let (tys, base) = match self.kind(t) {
            TyKind::Tuple(es) => (es, 0),
            TyKind::Adt(..) if self.is_class(t) => {
                let base = self.obj_field_base(t);
                (self.adt_field_tys(t), base)
            }
            TyKind::Adt(..) => (self.adt_field_tys(t), 0),
            k => ice(format_args!("field of {k:?}")),
        };
        let before = tys[..i as usize]
            .iter()
            .filter(|&&f| !self.is_unit(f))
            .count();
        base + before as u32
    }

    /// Base aggregate of a tagged enum/result: `{ tag }` sized to hold every variant view.
    fn tagged_base(&mut self, t: TyId, tag: Ty) -> AggId {
        let views = self.variant_views(t, tag);
        let (mut size, mut align) = (1, 1);
        for v in &views {
            let l = &self.aggs[v.0 as usize];
            size = size.max(l.size);
            align = align.max(l.align);
        }
        self.lay.views.insert(t, views);
        self.push_agg(AggLayout {
            name: self.type_name(t),
            size: size.next_multiple_of(align),
            align,
            fields: vec![(tag, 0)],
        })
    }

    fn variant_views(&mut self, t: TyId, tag: Ty) -> Vec<AggId> {
        let names: Vec<String> = match self.kind(t) {
            TyKind::Adt(d, _) => self
                .enum_def(d)
                .variants
                .iter()
                .map(|v| v.name.clone())
                .collect(),
            _ => vec!["Ok".into(), "Err".into()],
        };
        let base = self.type_name(t);
        let mut views = vec![];
        for (i, vname) in names.into_iter().enumerate() {
            let mut tys = vec![tag];
            for p in self.variant_tys(t, i as u32) {
                match self.ty(p) {
                    // `Result<void, E>`: the Ok view has no payload field.
                    Ty::Unit => {}
                    vt => tys.push(vt),
                }
            }
            views.push(self.new_agg(format!("{base}::{vname}"), &tys));
        }
        views
    }

    /// View aggregate of variant `v` of a tagged enum / result type.
    pub(super) fn view(&mut self, t: TyId, v: u32) -> AggId {
        self.value_agg(t);
        self.lay.views.get(&t).unwrap_or_else(|| ice("enum views"))[v as usize]
    }

    /// Heap object layout of a concrete class type.
    pub(super) fn obj_agg(&mut self, t: TyId) -> AggId {
        if let Some(&a) = self.lay.objs.get(&t) {
            return a;
        }
        let TyKind::Adt(d, _) = self.kind(t) else {
            ice("object layout of a non-class type")
        };
        let mut tys = vec![];
        if self.has_header(d) {
            tys.push(Ty::Ptr);
        }
        let fields = self.adt_field_tys(t);
        tys.extend(self.stored_tys(fields));
        let a = self.new_agg(format!("{} object", self.type_name(t)), &tys);
        self.lay.objs.insert(t, a);
        a
    }

    /// VIR field index of HIR field 0 in a class object (1 when there is a vtable pointer).
    pub(super) fn obj_field_base(&mut self, t: TyId) -> u32 {
        match self.kind(t) {
            TyKind::Adt(d, _) => self.has_header(d) as u32,
            _ => ice("object field of a non-class type"),
        }
    }

    /// Does the class hierarchy of `d` need a vtable pointer in every object?
    pub(super) fn has_header(&mut self, d: DefId) -> bool {
        let root = self.class_root(d);
        if self.lay.headers.is_none() {
            self.lay.headers = Some(self.hierarchy_headers());
        }
        self.lay.headers.as_ref().is_some_and(|h| h.contains(&root))
    }

    /// The roots of the class hierarchies that need a vtable pointer: one pass over all classes
    /// (scanning them per hierarchy was quadratic in the number of classes).
    fn hierarchy_headers(&self) -> HashSet<DefId> {
        let mut roots = HashSet::new();
        for i in 0..self.hir.defs.len() {
            let id = DefId(i as u32);
            if let hir::Def::Adt(a) = self.hir.def(id) {
                if a.kind == AdtKind::Class && (a.base.is_some() || !a.vtable.is_empty()) {
                    roots.insert(self.class_root(id));
                }
            }
        }
        roots
    }

    pub(super) fn class_root(&self, mut d: DefId) -> DefId {
        while let Some(b) = self.adt_def(d).base {
            match self.types.kind(b) {
                TyKind::Adt(bd, _) => d = *bd,
                _ => ice("base class is not a class type"),
            }
        }
        d
    }

    /// `{ count: u64, value: T }` of `shared<T>`.
    pub(super) fn shared_box(&mut self, inner: TyId) -> AggId {
        if let Some(&a) = self.lay.boxes.get(&inner) {
            return a;
        }
        let vt = self.ty(inner);
        let a = self.new_agg(format!("shared {}", self.type_name(inner)), &[Ty::U64, vt]);
        self.lay.boxes.insert(inner, a);
        a
    }

    /// Environment layout of closure `def` instantiated with `targs`.
    pub(super) fn env_agg(&mut self, def: DefId, targs: &[TyId]) -> AggId {
        let key = (def, targs.to_vec());
        if let Some(&a) = self.lay.envs.get(&key) {
            return a;
        }
        let f = self.fn_def(def);
        // A shared cell (`LocalDef::boxed`) is stored as its pointer, like a borrow.
        let caps: Vec<(PassMode, TyId)> = f
            .captures
            .iter()
            .map(|c| match f.body.locals[c.inner.0 as usize].boxed {
                true => (PassMode::Borrow, f.body.locals[c.inner.0 as usize].ty),
                false => (c.mode, f.body.locals[c.inner.0 as usize].ty),
            })
            .collect();
        let name = format!("{} env", f.name);
        // closure.rs `ENV_HEADER`: drop, clone and transfer entries.
        let mut tys = vec![Ty::Ptr, Ty::Ptr, Ty::Ptr];
        for (mode, t) in caps {
            let t = self.subst(t, targs);
            tys.push(match mode {
                PassMode::Borrow | PassMode::BorrowMut => Ty::Ptr,
                _ => self.ty(t),
            });
        }
        let a = self.new_agg(name, &tys);
        self.lay.envs.insert(key, a);
        a
    }
}

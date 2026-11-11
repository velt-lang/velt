//! Hashes of a program's types, functions and declarations, by structure and name
//! ([`super`]'s module docs). The definitions are destructured without `..`: a field added to
//! the HIR fails to build here until it is hashed or skipped with a reason.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rustc_hash::FxHashMap;

use crate::hir::{self, Def, DefId, FnDef, TyId, TyKind};

pub(super) fn finish(x: impl Hash) -> u64 {
    let mut s = DefaultHasher::new();
    x.hash(&mut s);
    s.finish()
}

/// Hashes of (name, hash) pairs, independent of their order.
pub(super) fn unordered(mut items: Vec<(String, u64)>) -> u64 {
    items.sort();
    finish(items)
}

/// Hashes of a program's types (memoized) and definitions.
pub(super) struct Hashing<'p> {
    pub p: &'p hir::Program,
    tys: FxHashMap<TyId, u64>,
}

impl<'p> Hashing<'p> {
    pub fn new(p: &'p hir::Program) -> Self {
        Self {
            p,
            tys: FxHashMap::default(),
        }
    }

    /// A definition's name: what stands for it in a hash.
    pub fn name(&self, d: DefId) -> &'p str {
        match self.p.defs.get(d.0 as usize) {
            Some(Def::Fn(f)) => &f.name,
            Some(Def::ExternFn(f)) => &f.name,
            Some(Def::Adt(a)) => &a.name,
            Some(Def::Enum(e)) => &e.name,
            Some(Def::Global(g)) => &g.name,
            Some(Def::Interface(i)) => &i.name,
            None => "",
        }
    }

    pub fn names(&self, ds: impl IntoIterator<Item = DefId>) -> Vec<&'p str> {
        ds.into_iter().map(|d| self.name(d)).collect()
    }

    pub fn sorted_names(&self, ds: impl IntoIterator<Item = DefId>) -> Vec<&'p str> {
        let mut names = self.names(ds);
        names.sort_unstable();
        names
    }

    pub fn opt_name(&self, d: Option<DefId>) -> Option<&'p str> {
        d.map(|d| self.name(d))
    }

    pub fn ty(&mut self, t: TyId) -> u64 {
        if let Some(&h) = self.tys.get(&t) {
            return h;
        }
        let mut s = DefaultHasher::new();
        let kind = self.p.types.kind(t);
        std::mem::discriminant(kind).hash(&mut s);
        match kind {
            TyKind::Int(i) => i.hash(&mut s),
            TyKind::Float(f) => f.hash(&mut s),
            TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Error
            | TyKind::Symbol => {}
            TyKind::Adt(d, args) | TyKind::Dyn(d, args) => {
                self.name(*d).hash(&mut s);
                self.tys_into(args, &mut s);
            }
            TyKind::Array(x) | TyKind::Shared(x) => self.ty(*x).hash(&mut s),
            TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => {
                (self.ty(*a), self.ty(*b)).hash(&mut s)
            }
            TyKind::Tuple(xs) => self.tys_into(xs, &mut s),
            TyKind::Option(x, n) => (self.ty(*x), n).hash(&mut s),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => {
                self.tys_into(params, &mut s);
                (self.ty(*ret), self.ty(*throws), rest).hash(&mut s);
            }
            TyKind::Closure(d) => self.name(*d).hash(&mut s),
            TyKind::Param(n) => n.hash(&mut s),
            TyKind::Literal(v) => v.hash(&mut s),
            TyKind::Simd(v) => v.hash(&mut s),
            TyKind::Op(op, args) => {
                op.hash(&mut s);
                self.tys_into(args, &mut s);
            }
        }
        let h = s.finish();
        self.tys.insert(t, h);
        h
    }

    fn tys_into(&mut self, ts: &[TyId], s: &mut DefaultHasher) {
        ts.len().hash(s);
        for &t in ts {
            self.ty(t).hash(s);
        }
    }

    pub fn tys(&mut self, ts: &[TyId]) -> Vec<u64> {
        ts.iter().map(|&t| self.ty(t)).collect()
    }

    pub fn opt_ty(&mut self, t: Option<TyId>) -> Option<u64> {
        t.map(|t| self.ty(t))
    }

    /// What other code observes of function `d`: its signature as sema finished it, and what the
    /// program's per-function tables say of it.
    pub fn function(&mut self, d: DefId, f: &FnDef) -> u64 {
        let FnDef {
            name: _, // The key of the summary.
            generics,
            params,
            ret,
            is_async,
            is_generator,
            self_ty,
            captures,
            shares_captures,
            changes_held,
            body,
            throws,
            span, // Moves with an edit before it; read only to find the function's shape.
        } = f;
        let p = self.p;
        let mut s = DefaultHasher::new();
        generics.hash(&mut s);
        for hir::Param { local, ty, mode } in params {
            // What the body does to the parameter: modifies it, or keeps it in a shared cell.
            let local = body.locals.get(local.0 as usize);
            let effects = local.map(|l| (l.mutable, l.boxed));
            (self.ty(*ty), mode, effects).hash(&mut s);
        }
        let sig = (self.ty(*ret), self.opt_ty(*throws), self.opt_ty(*self_ty));
        sig.hash(&mut s);
        (is_async, is_generator, shares_captures, changes_held).hash(&mut s);
        // `outer` and `inner` are locals of the enclosing function's body and of this one.
        for hir::Capture { mode, share, .. } in captures {
            (mode, share).hash(&mut s);
        }
        let fresh = p.fresh_closures.get(&d).map(|c| match *c {
            hir::FreshClosure::New(t) => (0u8, self.ty(t), 0),
            hir::FreshClosure::Several(t) => (1, self.ty(t), 0),
            hir::FreshClosure::Converts { from, to } => (2, self.ty(from), self.ty(to)),
        });
        fresh.hash(&mut s);
        // What a wrapper sema made stands for: a local of the function making the wrapper (which
        // the wrapper captures; the wrapper is that function's own closure, excused when it is
        // edited), or a module-level function at type arguments.
        let wraps = p.fn_wrappers.get(&d).map(|w| match w {
            hir::FnWrapped::Local(l) => (0u8, l.0 as u64, None, vec![]),
            hir::FnWrapped::Fn(f, args) => (1, 0, Some(self.name(*f)), self.tys(args)),
        });
        wraps.hash(&mut s);
        p.lock_free_callbacks.contains(&d).hash(&mut s);
        p.cpu_targets.get(&d).hash(&mut s);
        let shape = p.fn_shapes.get(span);
        let shape = shape.map(|sh| (&sh.name, sh.length, self.opt_name(sh.wraps)));
        shape.hash(&mut s);
        s.finish()
    }

    fn fields(&mut self, fields: &[hir::FieldDef]) -> Vec<u64> {
        (fields.iter())
            .map(|f| {
                let hir::FieldDef {
                    name,
                    ty,
                    default, // An expression: a declaration change detection sees.
                    optional,
                    private,
                    presence,
                    hidden,
                } = f;
                let flags = (optional, private, presence, hidden);
                finish((name, self.ty(*ty), default.is_some(), flags))
            })
            .collect()
    }

    /// One definition other than a function: its name and hash.
    pub fn declaration(&mut self, d: &Def) -> Option<(String, u64)> {
        Some(match d {
            Def::Fn(_) => return None,
            Def::ExternFn(f) => {
                let hir::ExternFnDef {
                    name,
                    symbol,
                    params,
                    ret,
                    is_async,
                    system,
                    host,
                    c,
                    is_decorator,
                    span: _,
                } = f;
                let sig = (self.tys(params), self.ty(*ret), is_async, is_decorator);
                // `CImport` and `HostImport` hold no ids: their `Debug` text is their content.
                // (HIR contract types without `Hash`: if one gains a `TyId` or `DefId`, hash it
                // here by structure instead. The same holds for `CTy`, `CValue` and `HookSet`.)
                let how = (system, format!("{host:?}"), format!("{c:?}"));
                (name.clone(), finish((0u8, symbol, sig, how)))
            }
            Def::Adt(a) => (a.name.clone(), self.adt(a)),
            Def::Enum(e) => {
                let hir::EnumDef {
                    name,
                    generics,
                    variants,
                    is_copy,
                    is_union,
                    span: _,
                } = e;
                let variants: Vec<_> = (variants.iter())
                    .map(|v| {
                        let hir::VariantDef {
                            name,
                            payload,
                            discriminant,
                            str_value,
                        } = v;
                        finish((name, self.tys(payload), discriminant, str_value))
                    })
                    .collect();
                (
                    name.clone(),
                    finish((2u8, variants, generics, is_copy, is_union)),
                )
            }
            Def::Interface(i) => {
                let hir::InterfaceDef {
                    name,
                    generics,
                    fields,
                    methods,
                    span: _,
                } = i;
                let methods: Vec<_> = (methods.iter())
                    .map(|m| {
                        let hir::InterfaceMethodDef {
                            name,
                            default,
                            promise,
                            throws,
                        } = m;
                        let default = self.opt_name(*default);
                        finish((name, default, promise, self.opt_ty(*throws)))
                    })
                    .collect();
                let fields = self.fields(fields);
                (name.clone(), finish((3u8, generics, fields, methods)))
            }
            Def::Global(g) => {
                let hir::GlobalDef {
                    name,
                    ty,
                    init: _, // An expression: a declaration change detection sees.
                    span: _,
                    slot,
                    c_const,
                    c_global,
                } = g;
                // `CConst` and `CGlobal` hold no ids.
                let c = format!("{c_const:?}{c_global:?}");
                (name.clone(), finish((4u8, self.ty(*ty), slot, c)))
            }
        })
    }

    fn adt(&mut self, a: &hir::AdtDef) -> u64 {
        let hir::AdtDef {
            name: _,
            kind,
            generics,
            fields,
            is_copy,
            declared_struct,
            c_struct,
            private_fields,
            opaque,
            assigned,
            base,
            inherited_inits,
            ctor,
            dispose,
            to_string,
            to_json,
            inspect,
            vtable,
            span: _,
        } = a;
        let flags = (is_copy, declared_struct, c_struct);
        let more = (private_fields, opaque, assigned);
        let hooks = [ctor, dispose, to_string, to_json, inspect].map(|h| self.opt_name(*h));
        // Where each inherited initializer runs; the initializer is a declaration.
        let inits: Vec<_> = (inherited_inits.iter())
            .map(|i| (i.field, i.depth, i.at))
            .collect();
        let shape = (
            format!("{kind:?}"),
            generics,
            self.fields(fields),
            flags,
            more,
        );
        let base = self.opt_ty(*base);
        let vtable = self.names(vtable.iter().copied());
        finish((1u8, shape, base, hooks, inits, vtable))
    }

    /// A decorator's constant argument, with enum members named.
    pub fn tree(&self, t: &hir::ConstTree, s: &mut DefaultHasher) {
        use hir::ConstTree as C;
        std::mem::discriminant(t).hash(s);
        match t {
            C::Null | C::Undefined => {}
            C::Bool(b) => b.hash(s),
            C::Int(i) => i.hash(s),
            C::Float(f) => f.hash(s),
            C::Str(x) => x.hash(s),
            C::Array(xs) => {
                xs.len().hash(s);
                xs.iter().for_each(|x| self.tree(x, s));
            }
            C::Object(fields) => {
                fields.len().hash(s);
                for (k, v) in fields {
                    k.hash(s);
                    self.tree(v, s);
                }
            }
            C::EnumMember(d, i) => (self.name(*d), i).hash(s),
        }
    }
}

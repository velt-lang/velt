//! Object spreads and conversions in generic bodies whose fields are known only per
//! instantiation (#350, docs/internals/design/deferred-types.md "Object literals and spreads at
//! a deferred type"):
//!
//! - `{ ...t, $set }` with `t: T` is the deferred type `TyOp::Spread` over its parts' types (each
//!   explicit property a one-field object type), and its value is an `Intrinsic::SpreadObject`
//!   over the parts. At an instantiation it reduces to the object type JavaScript's spread
//!   builds ([`Ctx::reduce_spread`]): the parts' public fields in order, a later part's field
//!   overriding an earlier one while keeping its position, an optional field copied only while
//!   present.
//! - A value of a generic body used as a type that differs from its own while the type
//!   parameters are unknown (`s: T & { $set }` as a `T`, `{ ...t }` as a `T`) is an `Upcast`,
//!   the same value seen as the other type, with the requirement `TyOp::Convert`: at each
//!   instantiation both must be one object type ([`Ctx::reduce_convert`]). Object types have
//!   fixed layouts, so a value can't be seen as a type with other fields without a copy, and a
//!   copy would be a different object where TypeScript passes the same one: such an
//!   instantiation is an error.

use velt_common::{Diagnostic, Span};

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{AdtKind, Nothing, TyId, TyKind, TyOp};

/// What a deferred type that isn't an alias puts between its arguments when shown.
pub(crate) fn op_sep(o: TyOp) -> &'static str {
    match o {
        TyOp::Spread => ", ...",
        TyOp::Convert(_) => " as ",
        _ => " & ",
    }
}

impl Ctx<'_> {
    /// Deferred type `t` (`o` over the known `args`, not an alias) reduced.
    pub(crate) fn reduce_part_op(&mut self, o: TyOp, t: TyId, args: Vec<TyId>, span: Span) -> TyId {
        match o {
            TyOp::Spread => self.reduce_spread(t, args, span),
            TyOp::Convert(c) => self.reduce_convert(c, args, span),
            _ => self.reduce_intersection(t, args, span),
        }
    }

    /// The deferred spread `op` (`TyOp::Spread`) with known parts `args`, reduced: the object
    /// type of the merged fields, or `Error` (reported at `span`) when a part isn't an object.
    pub(crate) fn reduce_spread(&mut self, op: TyId, args: Vec<TyId>, span: Span) -> TyId {
        let module = self.type_level.op_modules.get(&op).copied().unwrap_or(0);
        let mut out: Vec<ShapeField> = vec![];
        for part in args {
            // A class's optional field is an own property even while unset, so it is always
            // copied, its `undefined` too (`anon::has_presence`).
            let class = matches!(self.ty.kind(part), TyKind::Adt(d, _)
                if self.adt(*d).is_some_and(|a| a.kind == AdtKind::Class));
            let Some(fields) = self.spread_part_fields(part) else {
                let tn = self.display(part);
                let mut d = Diagnostic::error(
                    format!("cannot spread a value of type `{tn}` into an object"),
                    span,
                );
                let optional = self
                    .ty
                    .opt_payload(part)
                    .is_some_and(|p| self.spread_part_fields(p).is_some());
                d = match optional {
                    // `...x` with `x: T | undefined` (JavaScript spreads nothing when it is
                    // missing): its fields are known only per instantiation.
                    true => d.with_note("in this generic function the spread's source may be `undefined` or `null`, and its type has a type parameter: such a spread is not supported yet (#350); test the source first (`x === undefined ? { ...a } : { ...a, ...x }`)"),
                    false => d.with_note("in this generic function the spread's source is a type parameter; at this instantiation it is not an object type (object spread works on structs, classes and object types)"),
                };
                if self.is_method_iface(part) {
                    d = d.with_note(format!("`{tn}` is an interface with methods: a spread copies an object's own properties, which are its methods too for an object literal but not for a class instance, and an interface value can be either; spread a value of an object type, or build the object from the fields (`{{ a: x.a }}`)"));
                }
                self.error(d);
                return self.ty.error;
            };
            for f in fields {
                match out.iter_mut().find(|g| g.name == f.name) {
                    // An optional field overrides the earlier value only while present, so the
                    // field keeps the earlier one's optionality and type (`F` under a later
                    // `F | undefined`), as TypeScript types it and as for a concrete spread
                    // (`spread_object`); a present `undefined` over a required field is a
                    // `TypeError` (lowering). TypeScript's `number | string` for one of another
                    // type is a union Velt doesn't build here.
                    Some(g) if f.optional => {
                        let read = self.ty.option_of(f.ty, Nothing::Undefined);
                        // `a?: F | undefined` (a field of a source that may be missing) over
                        // `a: F`: absent, the earlier value stays; a present `undefined` is a
                        // `TypeError` (lowering), as for a concrete spread.
                        let payload = self.ty.opt_payload(f.ty).map(|p| self.canon(p));
                        let fits = (class && self.canon(g.ty) == self.canon(read))
                            || payload == Some(self.canon(g.ty));
                        if self.canon(g.ty) != self.canon(f.ty) && !fits {
                            let (gt, ft) = (self.display(g.ty), self.display(f.ty));
                            let msg = format!("this generic function spreads an optional field `{}` of type `{ft}` over an earlier value of type `{gt}`, which is not supported yet at this instantiation (#350)", f.name);
                            self.error(Diagnostic::error(msg, span).with_note(
                                "JavaScript keeps the earlier value while the field is absent, so the field would hold either type; give both the same type",
                            ));
                            return self.ty.error;
                        }
                        g.readonly = false;
                    }
                    Some(g) => {
                        g.ty = f.ty;
                        g.optional = false;
                        g.readonly = false;
                    }
                    None => out.push(ShapeField {
                        readonly: false,
                        ..f
                    }),
                }
            }
        }
        self.anon_type_with(&out, module)
    }

    /// Can a spread of object type `from` be built as object type `to`: the same field names,
    /// each of `to`'s type, or of that type or `undefined` (a present `undefined` is a
    /// `TypeError` then, as for a concrete spread with an expected type)?
    fn spread_fits(&mut self, from: TyId, to: TyId) -> bool {
        let (have, want) = (self.public_fields(from), self.public_fields(to));
        if have.len() != want.len() || !self.is_object_type(to) {
            return false;
        }
        want.iter().all(|w| {
            have.iter().find(|h| h.name == w.name).is_some_and(|h| {
                let (ht, wt) = (self.canon(h.ty), self.canon(w.ty));
                let relaxed = self.ty.option_of(w.ty, Nothing::Undefined);
                (ht == wt || ht == self.canon(relaxed)) && (w.optional || !h.optional)
            })
        })
    }

    /// The fields a spread copies out of a value of concrete type `t` (its public fields, not
    /// ES private ones or hidden brands), or `None` when `t` is not an object type that lowering
    /// can read fields of by name.
    pub(crate) fn spread_part_fields(&mut self, t: TyId) -> Option<Vec<ShapeField>> {
        let TyKind::Adt(d, _) = self.ty.kind(t).clone() else {
            return None;
        };
        let kind = self.adt(d)?.kind;
        if !matches!(kind, AdtKind::Anon | AdtKind::Struct | AdtKind::Class)
            || Some(d) == self.prelude_adt("Record")
        {
            return None;
        }
        let hidden = self.hidden_keys();
        let mut fields = self.public_fields(t);
        fields.retain(|f| {
            !f.name.starts_with(velt_syntax::ast::PRIVATE_NAME_PREFIX) && !hidden.contains(&f.name)
        });
        Some(fields)
    }

    /// The requirement `TyOp::Convert` number `c` with known arguments `[from, to]`: `to` when
    /// a value of `from` is a `to` without a copy (or, for a spread built as `to`, has its
    /// fields), else `Error`, reported at `span`.
    pub(crate) fn reduce_convert(&mut self, c: u32, args: Vec<TyId>, span: Span) -> TyId {
        let [from, to] = args[..] else {
            return self.ty.error;
        };
        let (a, b) = (self.canon(from), self.canon(to));
        if a == b || self.same_layout(a, b) {
            return to;
        }
        if self.type_level.spread_into.contains(&c) && self.spread_fits(a, b) {
            return to;
        }
        let (fa, tb) = (self.display(a), self.display(b));
        self.error(
            Diagnostic::error(
                format!("this generic function uses a `{fa}` as a `{tb}`, which are different object types at this instantiation"),
                span,
            )
            .with_note("TypeScript passes the same object; Velt's object types have fixed layouts, so the value would have to be copied, and a copy is a different object (#350)")
            .with_note("help: give the type parameter the fields the value has, or build the value the function returns from its fields"),
        );
        self.ty.error
    }
}

//! A finished unit's checking context, encoded whole (#2943 M2): [`encode_unit`] and
//! [`decode_unit`]. What a later unit's stage reads of the context is saved (the fields marked
//! persistent in the M2 inventory: definitions, types, scopes, interning tables, the passes'
//! summaries, ...); what is a memo, per-check state or the app's alone is not, and a decoded
//! context has it as [`Ctx::over`] makes it. The context is destructured field by field
//! ([`unit_fields!`]), so a field added to `Ctx` fails to compile until it is saved or skipped
//! here.
//!
//! The encoding: [`FORMAT`], the unit's file table (each file as the module it is the file of),
//! then the fields in order. The modules themselves are the caller's ([`decode_unit`]).

use velt_common::{Diagnostic, FileId, Fix, Label, Severity};

use super::{Arena, Codec, Reader, Writer};
use crate::ctx::{Ctx, DefTable};
use crate::hir::{TyId, TyTable};
use crate::types::Types;
use crate::SourceModule;

/// The version of the encoding: the first bytes of every encoded unit.
pub(crate) const FORMAT: &[u8; 8] = b"vunit\x00\x00\x01";

/// The fields of `Ctx` a unit saves (`save`), in order, then those it does not (`skip`: as
/// `Ctx::over` makes them). Generates the field walk of the encoder and decoder.
macro_rules! unit_fields {
    (save: [$($p:ident),* $(,)?], special: [$($sp:ident),* $(,)?], skip: [$($s:ident),* $(,)?] $(,)?) => {
        /// Encode the saved fields of `cx`, with the bytes each took.
        fn enc_fields(cx: &Ctx, w: &mut Writer, sizes: &mut Vec<(&'static str, usize)>) {
            let Ctx { $($p,)* $($sp,)* $($s: _,)* } = cx;
            $(
                let at = w.out.len();
                Codec::enc($p, w);
                sizes.push((stringify!($p), w.out.len() - at));
            )*
            $(
                let at = w.out.len();
                special::enc(stringify!($sp), $sp, w);
                sizes.push((stringify!($sp), w.out.len() - at));
            )*
        }

        fn dec_fields(cx: &mut Ctx, r: &mut Reader) {
            $(cx.$p = Codec::dec(r);)*
            $(special::dec(stringify!($sp), &mut cx.$sp, r);)*
        }
    };
}

unit_fields! {
    save: [
        fn_shapes, ty, defs, info, def_spans, unit_sums, scopes, prelude, prelude_type_keys,
        aliases, extensions, impls, iface_writes, part_ifaces, synth_ifaces, generic_fns,
        generic_called, union_views, any, ext_impls, ext_by_iface, anon, readonly_twins,
        any_unknown_views, objects, brands, brand_keys, generic_brands, unique_symbols,
        record_views, field_only, field_only_of, primitive_guards, writes_undefined,
        nothing_slots, shaped, laid_out, counted_objects, changes_held, lock_free_callbacks,
        module_scripts, state_refused, lazy, unions, recursive_unions, generic_recursive,
        recursive_union_insts, alias_names, generic_overrides, getter_only_setters,
        abstract_methods, defined_setters, defaults_checked, fills, default_throws, object_impls,
        hook_adapters, field_defaults_checked, closure_counts, fn_defs, fn_values, frozen_reads,
        cpu_targets, provisional_slots, literal_ifaces, literal_structs, iface_adapters,
        dyn_iface_targets, fn_views, fn_sites, promise_forwarders, nested, nested_locals,
        decorator_uses, c_exports, enclosing_class, static_this, static_copies, static_copy_of,
        generator_values, overload_sets, callback_wrappers, kept_fns, kept_consts, symbol_keys,
        held_borrows_used, object_copies, method_facts, super_calls, type_defaults, diags,
        widening_values, recursive_union_instances,
    ],
    // Saved in part (`special`).
    special: [sigq, type_level],
    skip: [
        // The caller's (`decode_unit`), or set again by every check that uses the unit.
        modules, root, gen, units, units_stage, unit_base, import_targets, loads, loads_added,
        stack_base, stack_budget, held_borrows, ide, body_records, source_facts, std_missed,
        kept_demand,
        // Memos, built again on demand.
        marked_aliases, extension_names, impl_index, ext_memo, ext_exists_memo, ext_memo_impls,
        canon_memo, canon_errors, canon_keys, primitive_statics, classes_below, groups,
        shared_memo, pure_fns, subclasses, js_api_modules, native_libraries, channel_sends_memo,
        uses_this_fn, decorator_scopes, export_memo,
        // State within one pass or one body's check, empty or spent when a unit is finished.
        bad_key_sites, closure_constraints, adapter_uses, in_blanket, impl_scope,
        narrowed_fields, shapes_done, query_locals, bodies_ok, checking_unused_aliases, shaping,
        assigned_objects, replaced_unions, module_slots, global_cycles, fn_value_depth,
        thread_local_uses, typing_globals, display_params, dropped_closures, frozen_elems,
        loop_roots, literal_hosts, literal_display, throw_checks, collected_defs,
        iface_generators, copy_diags, copy_uses, unwraps, reported_captures, reported_mutations,
        in_shared, crossing_callbacks, matching_context, checking, ret_checks, rec, overridden,
        moved_consts, borrow_pass_errors, fresh_checks, param_field_writes,
        param_proxy_targets, deferred_ts_returns, rechecks, closure_defs, literal_types,
        literal_found, late_consts, group_defs,
        // What the source rewrites found, installed again by every check (`rewrite_facts`).
        promoted_consts, asserted_rmw, global_props, generic_arrow_fns, generic_arrow_all,
        unsafe_arrow_fns, alias_blocked, kept_nested, closure_groups, group_names,
        has_overloads, gen_throw, overloaded_methods, ns_private, global_namespaces, recovered,
        // The app's alone: empty in std's unit.
        jsx_providers, jsx_refusals, poisoned, added_defs,
    ],
}

/// The fields saved in part.
mod special {
    use super::{Codec, Reader, Writer};
    use crate::type_level;
    use crate::type_queries::SigQueries;

    pub(super) trait Part {
        fn enc_part(&self, w: &mut Writer);
        fn dec_part(&mut self, r: &mut Reader);
    }

    pub(super) fn enc(_name: &str, v: &impl Part, w: &mut Writer) {
        v.enc_part(w);
    }

    pub(super) fn dec(_name: &str, v: &mut impl Part, r: &mut Reader) {
        v.dec_part(r);
    }

    /// The deferred signatures are filled and the flags reset once collection is done: only
    /// the read-only parameters stay.
    impl Part for SigQueries<'_> {
        fn enc_part(&self, w: &mut Writer) {
            let SigQueries {
                sig_fill: _,
                sig_deferred: _,
                deferred_sigs: _,
                filling_sigs: _,
                readonly_params,
                erase_generics: _,
                in_field: _,
                field_erased: _,
            } = self;
            readonly_params.enc(w);
        }
        fn dec_part(&mut self, r: &mut Reader) {
            self.readonly_params = Codec::dec(r);
        }
    }

    /// The deferred types and what is known of them; not the state of an expansion in progress,
    /// nor the memo of property-name kinds.
    impl Part for type_level::State {
        fn enc_part(&self, w: &mut Writer) {
            let type_level::State {
                alias_site: _,
                use_site: _,
                key_kinds: _,
                deferring: _,
                stuck: _,
                callable_params: _,
                op_sites,
                failed_sites,
                op_modules,
                intersection_parts,
                converts,
                spread_into,
                reduced,
                inline,
                alias_canon,
                alias_shapes,
            } = self;
            op_sites.enc(w);
            failed_sites.enc(w);
            op_modules.enc(w);
            intersection_parts.enc(w);
            converts.enc(w);
            spread_into.enc(w);
            reduced.enc(w);
            inline.enc(w);
            alias_canon.enc(w);
            alias_shapes.enc(w);
        }
        fn dec_part(&mut self, r: &mut Reader) {
            self.op_sites = Codec::dec(r);
            self.failed_sites = Codec::dec(r);
            self.op_modules = Codec::dec(r);
            self.intersection_parts = Codec::dec(r);
            self.converts = Codec::dec(r);
            self.spread_into = Codec::dec(r);
            self.reduced = Codec::dec(r);
            self.inline = Codec::dec(r);
            self.alias_canon = Codec::dec(r);
            self.alias_shapes = Codec::dec(r);
        }
    }
}

/// `cx`, a finished unit, encoded.
pub(crate) fn encode_unit(cx: &Ctx) -> Vec<u8> {
    encode_unit_sized(cx).0
}

/// [`encode_unit`], with the bytes each saved field took (and last, how many of them are AST
/// fragments).
pub(crate) fn encode_unit_sized(cx: &Ctx) -> (Vec<u8>, Vec<(&'static str, usize)>) {
    let mut w = Writer::new();
    let mut sizes = vec![];
    // The modules' files first, in order: a decoder maps them to the modules it is given.
    for m in &cx.modules {
        w.file(m.file);
    }
    enc_fields(cx, &mut w, &mut sizes);
    sizes.push(("(of which AST)", w.ast_bytes));
    let (body, files) = w.finish();
    let mut head = Writer::new();
    head.out.extend_from_slice(FORMAT);
    head.uint(cx.modules.len() as u64);
    head.uint(files.len() as u64);
    // A file that is no module's (none so far) keeps its id, which only this session knows.
    for &f in &files[cx.modules.len().min(files.len())..] {
        head.uint(u64::from(f.0));
    }
    let mut out = head.out;
    out.extend_from_slice(&body);
    (out, sizes)
}

/// The unit [`encode_unit`] encoded, over `modules` (the unit's modules, in the order it had
/// them, with this session's files), its AST nodes decoded into `arena`; `None` when `bytes` is
/// not a whole encoding of this [`FORMAT`] or not of as many modules. The fields a unit does not
/// save are as [`Ctx::over`] makes them; the definitions are frozen and the unit's base is after
/// them, as `units::finish_unit` left them.
///
/// Runs on a thread with sema's stack (decoding nests as deep as the unit's expressions; the
/// fragments are decoded right there, `velt_syntax::with_stack_of`).
///
/// # Safety
///
/// `arena` must outlive the context returned and every copy of it ([`Arena`]).
pub(crate) unsafe fn decode_unit<'m>(
    bytes: &[u8],
    arena: &Arena,
    modules: Vec<&'m SourceModule>,
) -> Option<Ctx<'m>> {
    let rest = bytes.strip_prefix(FORMAT.as_slice())?;
    let mut head = Reader::new(rest, &[]);
    let n = head.len();
    let nfiles = head.len();
    if n != modules.len() || nfiles < n {
        return None;
    }
    let mut files: Vec<FileId> = modules.iter().map(|m| m.file).collect();
    for _ in n..nfiles {
        files.push(FileId(head.u32()));
    }
    if head.failed {
        return None;
    }
    let body = &rest[head.pos..];
    let mut cx = Ctx::over(modules, 0);
    let done = velt_syntax::with_stack_of(crate::SEMA_STACK_BYTES, || {
        // SAFETY: the caller keeps `arena` alive longer than `cx` (above).
        let mut r = unsafe { Reader::with_arena(body, &files, arena) };
        for i in 0..n {
            if r.uint() != i as u64 {
                return false;
            }
        }
        dec_fields(&mut cx, &mut r);
        r.done()
    });
    if !done {
        return None;
    }
    cx.freeze_defs();
    cx.unit_base = cx.defs.len() as u32;
    Some(cx)
}

/// The type table as its kinds in order: decoding interns them again, which gives each its id
/// back (a damaged table that does not is refused) and rebuilds the intern map.
impl Codec for Types {
    fn enc(&self, w: &mut Writer) {
        let Types {
            table,
            unit,
            never,
            error,
            bool_,
            str_,
            i32,
            i64,
            u64,
            usize,
            f64,
            bigint_named,
            inferred_fns,
        } = self;
        w.uint(table.len() as u64);
        for i in 0..table.len() as u32 {
            table.kind(TyId(i)).enc(w);
        }
        for t in [unit, never, error, bool_, str_, i32, i64, u64, usize, f64] {
            t.enc(w);
        }
        bigint_named.enc(w);
        inferred_fns.enc(w);
    }

    fn dec(r: &mut Reader) -> Self {
        let n = r.len();
        let mut table = TyTable::new();
        for i in 0..n as u32 {
            let k = Codec::dec(r);
            if r.failed {
                break;
            }
            if table.intern(k) != TyId(i) {
                r.fail();
                break;
            }
        }
        let mut ids = [TyId(0); 10];
        for t in &mut ids {
            *t = TyId::dec(r);
        }
        let [unit, never, error, bool_, str_, i32, i64, u64, usize, f64] = ids;
        Types {
            table,
            unit,
            never,
            error,
            bool_,
            str_,
            i32,
            i64,
            u64,
            usize,
            f64,
            bigint_named: Codec::dec(r),
            inferred_fns: Codec::dec(r),
        }
    }
}

impl<T: Codec> Codec for DefTable<T> {
    fn enc(&self, w: &mut Writer) {
        let items = self.items_all();
        w.uint(items.len() as u64);
        for x in items {
            x.enc(w);
        }
    }

    fn dec(r: &mut Reader) -> Self {
        let mut t = DefTable::default();
        for x in Vec::<T>::dec(r) {
            t.push(x);
        }
        t
    }
}

super::structs! {
    Diagnostic { severity, message, labels, notes, code, fixes }
    Label { span, message }
    Fix { title, edits, applicability }
}

super::enums! {
    Severity {
        0 => Error,
        1 => Warning,
        2 => Note,
    }
    Applicability {
        0 => Safe,
        1 => Review,
    }
}

use velt_common::Applicability;

impl Codec for Box<str> {
    fn enc(&self, w: &mut Writer) {
        String::from(&**self).enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        String::dec(r).into_boxed_str()
    }
}

//! What the source rewrites and scans of a check found (`check_timed`), as one value: installed
//! in each context the check makes ([`Found::install`]), and keyed for std's unit kept between
//! checks ([`Found::std_key`], `crate::std_unit`).

use rustc_hash::FxHashSet;
use velt_common::{Diagnostic, FileId, Span};

use crate::ctx::Ctx;
use crate::{std_unit, units, CheckOptions, SourceModule};

pub(crate) struct Found<'a> {
    pub(crate) ns_private: &'a [crate::namespaces::Private],
    pub(crate) global_ns: &'a crate::namespaces::GlobalNamespaces,
    pub(crate) gen_throw: bool,
    pub(crate) alias_blocked: &'a crate::recursive_aliases::Blocked,
    pub(crate) lifted: Option<&'a crate::generic_arrows::Lifted>,
    pub(crate) overloaded: Option<&'a crate::overloads::Rewritten>,
    pub(crate) null_proto: bool,
    pub(crate) record_accessors: bool,
    pub(crate) ctor_reads: bool,
    pub(crate) brands: &'a crate::defined_props::Brands,
    pub(crate) global_props: &'a crate::global_this::GlobalProps,
    /// The modules the parser recovered, with their source text.
    pub(crate) recovered: &'a [(usize, &'a str)],
    pub(crate) merge_diags: &'a [Diagnostic],
    pub(crate) promoted: &'a FxHashSet<Span>,
    pub(crate) rmw: &'a FxHashSet<Span>,
    pub(crate) closures: Option<&'a crate::nested_closures::Rewritten>,
}

impl Found<'_> {
    /// Install what was found in `cx`: a fresh context, or the app's copy of std's unit, which
    /// may come from another check (`std_unit`); not the diagnostics in std's unit (`diags`
    /// false: the app's copy has them).
    pub(crate) fn install(&self, cx: &mut Ctx, diags: bool) {
        cx.ns_private = self.ns_private.to_vec();
        cx.global_namespaces = self.global_ns.clone();
        cx.gen_throw = self.gen_throw;
        cx.alias_blocked = self.alias_blocked.clone();
        if let Some(l) = self.lifted {
            cx.generic_arrow_fns = l.local_fns.clone();
            cx.generic_arrow_all = l.all_fns.clone();
            cx.unsafe_arrow_fns = l.unsafe_fns.clone();
        }
        if let Some(o) = self.overloaded {
            cx.has_overloads = true;
            cx.overloaded_methods = o.methods.clone();
        }
        cx.objects.null_proto_records = self.null_proto;
        cx.objects.record_accessors = self.record_accessors;
        cx.objects.ctor_reads = self.ctor_reads;
        cx.objects.brands = self.brands.clone();
        cx.global_props = self.global_props.clone();
        cx.recovered = (self.recovered.iter())
            .map(|&(m, src)| (m, crate::collect::exports::words(src)))
            .collect();
        if diags {
            cx.diags.extend(self.merge_diags.iter().cloned());
        }
        cx.promoted_consts = self.promoted.clone();
        cx.asserted_rmw = self.rmw.clone();
        if let Some(c) = self.closures {
            if diags {
                cx.diags.extend(c.diags.iter().cloned());
            }
            cx.kept_nested = c.kept.clone();
            cx.set_closure_groups(&c.groups);
        }
    }

    /// The key of std's unit for this check (`std_unit::key`): the caller's `std_key` for std's
    /// sources, with what this check hands std's stage. Of the spans, those in std's files (an
    /// edit of the program moves its own); of the per-module facts, std's modules'.
    pub(crate) fn std_key(
        &self,
        std_key: u64,
        opts: CheckOptions,
        order: &units::Order,
        modules: &[SourceModule],
    ) -> u64 {
        let files: FxHashSet<FileId> = (modules.iter())
            .filter(|m| m.is_std)
            .map(|m| m.file)
            .collect();
        let sp = |s: &FxHashSet<Span>| std_unit::std_spans(s, &files);
        let lifted = (self.lifted).map(|l| [sp(&l.local_fns), sp(&l.all_fns), sp(&l.unsafe_fns)]);
        let mut methods: Vec<&String> = self.overloaded.iter().flat_map(|o| &o.methods).collect();
        methods.sort();
        let blocked = std_unit::std_spans(self.alias_blocked.keys(), &files);
        let kept = self.closures.map(|c| sp(&c.kept));
        let groups: Vec<_> = (self.closures.iter().flat_map(|c| &c.groups))
            .filter(|g| g.arrow_spans.iter().any(|s| files.contains(&s.file)))
            .collect();
        let mut props = self.global_props.clone();
        props.remap(&|m| order.in_std(m));
        let mut brands = self.brands.clone();
        brands.remap(&|m| order.in_std(m));
        let recovered: Vec<_> = (self.recovered.iter())
            .filter(|r| order.in_std(r.0).is_some())
            .collect();
        let flags = (self.null_proto, self.record_accessors, self.ctor_reads);
        let parts: [&dyn std::fmt::Debug; 13] = [
            &(opts.eager_std, opts.native_executor, opts.require_main),
            &(flags, self.gen_throw),
            &lifted,
            &methods,
            &blocked,
            &(sp(self.promoted), sp(self.rmw)),
            &kept,
            &groups,
            &props,
            &brands,
            &order.std_facts(self.ns_private, self.global_ns),
            &recovered,
            &files.len(),
        ];
        std_unit::key(std_key, &parts)
    }
}

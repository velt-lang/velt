//! Every definition record and alias of std's frozen unit comes back from its encoding as it
//! was, but for its functions' bodies, which a unit does not keep.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use velt_common::Diagnostics;

use super::ast_refs::{Arena, AstNode};
use super::hook::{self, Hook, Inspect};
use super::{Codec, Reader, Writer};
use crate::ctx::Ctx;
use crate::defs::{AliasInfo, DefInfo, FnSource};
use crate::hir::Program;
use crate::script_locals::sema_tests::programs;
use crate::CheckOptions;

/// Check `src` by units, std's stage a check of its own, with `hook` installed.
pub(super) fn check_by_units(src: &str, hook: Hook) -> (Option<Program>, Diagnostics) {
    let loaded = programs::load_src(src);
    let opts = CheckOptions {
        units: true,
        eager_std: true,
        ..Default::default()
    };
    hook::with(hook, || {
        crate::check_with(&loaded.modules, loaded.root, opts)
    })
}

/// A hook that runs `f` on std's unit (once: a check may check std more than once) and keeps
/// what it returns.
pub(super) fn inspect_once<T: Send + 'static>(
    f: impl Fn(&Ctx) -> T + Send + 'static,
) -> (Inspect, Arc<Mutex<Option<T>>>) {
    let out = Arc::new(Mutex::new(None));
    let keep = out.clone();
    let inspect: Inspect = Box::new(move |cx: &Ctx| {
        let mut slot = keep.lock().unwrap();
        if slot.is_none() {
            *slot = Some(f(cx));
        }
    });
    (inspect, out)
}

/// `node` as a unit keeps it, in `a`.
fn kept_node<'x, T: AstNode>(node: &T, a: &'x Arena) -> &'x T {
    a.alloc(node.kept().into_owned())
}

/// `info` as a unit keeps it: its AST without functions' bodies.
fn kept<'x>(info: &DefInfo<'x>, a: &'x Arena) -> DefInfo<'x> {
    let mut info = info.clone();
    match &mut info {
        DefInfo::Fn(f) => {
            f.source = f.source.map(|s| match s {
                FnSource::Decl(d) => FnSource::Decl(kept_node(d, a)),
                FnSource::Default(s, b) => FnSource::Default(kept_node(s, a), kept_node(b, a)),
            });
            f.extern_sig = f.extern_sig.map(|s| kept_node(s, a));
        }
        DefInfo::Adt(x) => x.decl = x.decl.map(|d| kept_node(d, a)),
        DefInfo::Enum(x) => x.decl = x.decl.map(|d| kept_node(d, a)),
        DefInfo::Iface(x) => x.decl = x.decl.map(|d| kept_node(d, a)),
        DefInfo::Global(g) => {
            g.src.ann = g.src.ann.map(|t| kept_node(t, a));
            g.src.init = g.src.init.map(|e| kept_node(e, a));
            g.src.c_global = g.src.c_global.map(|(l, w)| (kept_node(l, a), w));
        }
    }
    info
}

/// `info` printed, its maps sorted (a decoded map holds the same entries in another order).
fn shown(mut info: DefInfo) -> String {
    fn sorted<V: std::fmt::Debug>(m: &mut rustc_hash::FxHashMap<String, V>) -> String {
        let mut v: Vec<String> = m.drain().map(|e| format!("{e:?}")).collect();
        v.sort();
        v.join(", ")
    }
    let maps = match &mut info {
        DefInfo::Adt(a) => [
            sorted(&mut a.methods),
            sorted(&mut a.vslots),
            sorted(&mut a.statics),
        ],
        _ => Default::default(),
    };
    format!("{info:?} {maps:?}")
}

/// What the round trip of the definition records found.
struct Found {
    infos: usize,
    aliases: usize,
    bytes: usize,
    nodes: usize,
    decode_ms: f64,
    differ: Vec<String>,
    deterministic: bool,
    again_same: bool,
}

fn encode_records(infos: &[DefInfo], aliases: &[AliasInfo]) -> (Vec<u8>, Vec<velt_common::FileId>) {
    let mut w = Writer::new();
    infos.to_vec().enc(&mut w);
    aliases.to_vec().enc(&mut w);
    w.finish()
}

fn round_trip(cx: &Ctx) -> Found {
    let (infos, aliases) = (cx.info.items_all(), &cx.aliases[..]);
    let (bytes, files) = encode_records(infos, aliases);
    let deterministic = encode_records(infos, aliases) == (bytes.clone(), files.clone());
    let arena = Arena::default();
    let start = Instant::now();
    let decoded = velt_syntax::with_stack_of(crate::SEMA_STACK_BYTES, || {
        // SAFETY: what is decoded is dropped in this function, before `arena`.
        let mut r = unsafe { Reader::with_arena(&bytes, &files, &arena) };
        let out: (Vec<DefInfo>, Vec<AliasInfo>) = (Codec::dec(&mut r), Codec::dec(&mut r));
        r.done().then_some(out)
    });
    let decode_ms = start.elapsed().as_secs_f64() * 1e3;
    let Some((out, out_aliases)) = decoded else {
        panic!("the definition records decode whole");
    };
    let again_same = encode_records(&out, &out_aliases).0 == bytes;
    let view = Arena::default();
    let mut differ = vec![];
    for (i, (a, b)) in infos.iter().zip(&out).enumerate() {
        let (a, b) = (shown(kept(a, &view)), shown(b.clone()));
        if a != b && differ.len() < 3 {
            differ.push(format!("def {i}:\n{a}\n---\n{b}"));
        }
    }
    for (i, (a, b)) in aliases.iter().zip(&out_aliases).enumerate() {
        let mut a = a.clone();
        a.decl = kept_node(a.decl, &view);
        let (a, b) = (format!("{a:?}"), format!("{b:?}"));
        if a != b && differ.len() < 3 {
            differ.push(format!("alias {i}:\n{a}\n---\n{b}"));
        }
    }
    if out.len() != infos.len() || out_aliases.len() != aliases.len() {
        differ.push("counts differ".into());
    }
    Found {
        infos: infos.len(),
        aliases: aliases.len(),
        bytes: bytes.len(),
        nodes: arena.len(),
        decode_ms,
        differ,
        deterministic,
        again_same,
    }
}

#[test]
fn std_s_definition_records_come_back_as_they_were() {
    let (inspect, found) = inspect_once(round_trip);
    let hook = Hook {
        inspect: Some(inspect),
        decoded: false,
    };
    let (program, diags) = check_by_units(super::hir_tests::SRC, hook);
    assert!(program.is_some(), "the program checks: {diags:?}");
    let found = found
        .lock()
        .unwrap()
        .take()
        .expect("std's unit was shown to the hook");
    assert!(found.infos > 1000, "std's definitions are there");
    assert!(found.differ.is_empty(), "{}", found.differ.join("\n\n"));
    assert!(
        found.deterministic,
        "the same records encode to the same bytes"
    );
    assert!(
        found.again_same,
        "the decoded records encode to the same bytes"
    );
    eprintln!(
        "units::codec: {} definition records and {} aliases: {} bytes ({} AST nodes), decoded in \
         {:.2} ms",
        found.infos, found.aliases, found.bytes, found.nodes, found.decode_ms
    );
}

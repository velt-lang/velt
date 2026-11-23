//! Compiling a prebuilt hook module's object (`prebuilt`'s module docs): the module, with what it
//! reaches, in a world of its own.
//!
//! The world is a program whose root sits in the std root, as a source generator host's does
//! (std code, which may import the hooks' internal module), and calls each `prebuilt` hook of the
//! module once: what the hooks need of std is loaded, checked and lowered as in any program, and
//! decided by nothing but std. After lowering, each hook's function is the object's entry point
//! under `prebuilt::symbol` (exported, with the parameter attributes no caller can be held to:
//! no `noalias`, `readonly` or `frozen`); every other function is internal to the object, and
//! the root's `main` goes. The rest is optimized with the entry points' callers unknown, as the
//! std cache's objects are, and compiled by LLVM into one object.
//!
//! Refused (no object, and builds load the module as before): an entry point whose signature is
//! not what `prebuilt.rs` says crosses, and code with state a program shares with it, which a
//! copy in the object would split in two: module state (`velt_rt_module_state`, the executor's
//! module hooks and the blocks of module_blocks.velt: [`holds_no_module_state`]), thread-local
//! values (`velt_tls_*`), static atomics (process statics such as `STR_EPOCH` among them) and
//! named statics. The exception is state of the module's own: the thread-local values and static
//! atomics declared in its private modules (`prebuilt.rs` "State"), which no program that links
//! the object loads ([`own_state`]), and the blocks of module state in module_blocks.velt's own
//! object.

use std::collections::HashSet;
use std::path::Path;

use velt_common::hook_sig::{Mode, Sig, T};
use velt_common::std_hooks::{prebuilt, HOOKS};
use velt_vir::vir::{self, Callee, Const, Linkage, Operand, Terminator, Ty};

use crate::driver::{check_program, BuildOptions, CheckScope, Session};

/// A compiled object, its entry points' signatures and the std files it was compiled from.
pub(super) struct Built {
    pub object: Vec<u8>,
    /// `prebuilt::entry_line` of each entry point.
    pub entries: Vec<String>,
    /// Build-stamp `file` entries of the std files its world loaded.
    pub files: Vec<String>,
    /// The externs of the thread-local values its code uses (its module's own: [`own_state`]),
    /// which the program linking it defines.
    pub tls: Vec<String>,
}

/// Compile the object of module `module_entry` (a std module table entry) for `target` and
/// `cpu`, from the std at `std_root`; with `heap_hooks`, allocating through mimalloc directly as
/// the builds that link it do (driver/heap_hooks.rs), else through the runtime's allocator.
pub(super) fn build(
    module_entry: &str,
    std_root: &Path,
    target: &str,
    cpu: Option<&str>,
    heap_hooks: bool,
) -> Result<Built, String> {
    let hooks: Vec<usize> = prebuilt::of_module(module_entry).map(|(i, _)| i).collect();
    let first = hooks
        .first()
        .map(|&i| &HOOKS[i])
        .ok_or("no prebuilt hook")?;
    let module = first.module();
    let spec = velt_common::std_modules::specifier(&module).ok_or("not a std module")?;
    let opts = BuildOptions {
        input: std_root.join(format!("__prebuilt_{module_entry}.velt")),
        root_source: Some(root_source(&hooks, &spec)),
        release: true,
        target: Some(target.to_string()),
        cpu: cpu.map(str::to_string),
        ..BuildOptions::default()
    };
    // Std code, checked as a generator host's root is; it links nothing.
    let mut sess = Session::new();
    sess.generator_build = true;
    let scope = CheckScope {
        require_main: true,
        ..CheckScope::default()
    };
    let checked = check_program(&mut sess, &opts, &scope);
    let hir =
        checked.map_err(|_| format!("its world does not check:\n{}", sess.render_diagnostics()))?;
    let own = own_state(&hir, prebuilt::private(module_entry));
    let files: Vec<String> = (sess.std_files.iter())
        .map(|&f| sess.sm.get(f))
        .filter(|f| f.path != opts.input)
        .map(|f| super::prebuilt::file_entry(&f.path, &f.src))
        .collect();
    let lower = velt_vir::LowerOptions {
        source_map: Some(&sess.sm),
        std_root: Some(std_root),
        native_inits: &[],
        cell_checks: false,
        debug_info: false,
        target: Some(target),
        shapes: false,
        no_cpu_extensions: false,
        counted_seed: None,
        jit: false,
        // Only builds that allocate through mimalloc directly link such an object (its lookup
        // key and a shipped object's manifest say which allocator it calls): a debug runtime
        // checks and counts every block (driver/heap_hooks.rs).
        heap_hooks,
    };
    let mut program = velt_vir::lower_checked(&hir, &lower).map_err(|d| {
        let shown: Vec<String> = d.iter().map(|d| d.render(&sess.sm)).collect();
        format!("its world does not lower:\n{}", shown.join("\n"))
    })?;
    drop(hir);
    program.files.clear();
    program.funcs.iter_mut().for_each(|f| f.locs.clear());
    let entries = entry_points(&mut program, &hooks)?;
    velt_opt::optimize(&mut program, velt_opt::OptLevel::Speed);
    // The entry stays declared (`dead_funcs` drops a declaration nothing calls).
    if !program.funcs.iter().any(|f| f.symbol == "velt_main") {
        let main = vir::Function::new("velt_main".into(), vec![], Ty::I32, Linkage::Import);
        program.funcs.push(main);
    }
    shares_nothing(&program, &own)?;
    holds_no_module_state(&program, prebuilt::private(module_entry))?;
    let tls = thread_locals(&program);
    velt_vir::verify(&program).map_err(|errs| errs.join("; "))?;
    let cg = velt_codegen_cl::CodegenOptions {
        target: target.to_string(),
        optimize: true,
        cpu: cpu.map(str::to_string),
    };
    let mut steps = vec![];
    let (objects, _) =
        velt_codegen_llvm::emit_objects_cached(&program, &cg, Some(1), &mut steps, None)?;
    let object = objects.into_iter().next().ok_or("no object")?;
    Ok(Built {
        object,
        entries,
        files,
        tls,
    })
}

/// The world's root: it imports the hooks of `hooks` from `spec` and calls each once, with
/// zeros and empty strings (its code is dropped; it only makes them live).
fn root_source(hooks: &[usize], spec: &str) -> String {
    let items: Vec<&str> = hooks.iter().map(|&i| HOOKS[i].item).collect();
    let mut src = format!(
        "import {{ {} }} from \"{spec}\";\n\nfunction main() {{\n",
        items.join(", ")
    );
    for &i in hooks {
        let Sig::Fn(f) = &HOOKS[i].sig else { continue };
        let args: Vec<&str> = (f.params.iter())
            .map(|p| if p.ty == T::Str { "\"\"" } else { "0" })
            .collect();
        src.push_str(&format!("  {}({});\n", HOOKS[i].item, args.join(", ")));
    }
    src.push_str("}\n");
    src
}

/// Make each hook of `hooks` an exported entry point under its symbol, and every other function
/// internal (the root's `main` with them, which nothing calls then): their `entry` lines.
fn entry_points(program: &mut vir::Program, hooks: &[usize]) -> Result<Vec<String>, String> {
    let mut entries = vec![];
    let mut is_entry = vec![false; program.funcs.len()];
    for &i in hooks {
        let h = &HOOKS[i];
        let mangled = velt_vir::mangle::mangle(&h.qualified());
        let at = (program.funcs.iter())
            .position(|f| f.symbol == mangled && !f.blocks.is_empty())
            .ok_or_else(|| format!("`{}` was not lowered", h.qualified()))?;
        let f = &mut program.funcs[at];
        let Sig::Fn(sig) = &h.sig else {
            return Err(format!("`{}` is no function", h.name));
        };
        if !crosses(sig, &f.params, f.ret) {
            return Err(format!(
                "`{}` was lowered as {:?} -> {:?}",
                h.name, f.params, f.ret
            ));
        }
        f.symbol = prebuilt::symbol(h);
        f.linkage = Linkage::Export;
        // Its callers are any program's: no whole-program facts about its pointers.
        for a in &mut f.param_attrs {
            a.noalias = false;
            a.readonly = false;
            a.frozen = false;
        }
        entries.push(super::prebuilt::entry_line(&f.symbol, &f.params, f.ret));
        is_entry[at] = true;
    }
    for (f, entry) in program.funcs.iter_mut().zip(is_entry) {
        if !entry && f.linkage == Linkage::Export {
            f.linkage = Linkage::Internal;
        }
    }
    Ok(entries)
}

/// Was a hook of signature `sig` lowered as `prebuilt.rs` says it crosses: each number or `bool`
/// a scalar, each `&string` a pointer, a `string` result through a trailing out-pointer?
fn crosses(sig: &velt_common::hook_sig::FnSig, params: &[Ty], ret: Ty) -> bool {
    let out = sig.ret == T::Str;
    if params.len() != sig.params.len() + usize::from(out) {
        return false;
    }
    // A `bool` as `Ty::Bool`, a number as a number.
    let scalar = |ty: &T, t: Ty| match ty {
        T::Bool => t == Ty::Bool,
        _ => !matches!(t, Ty::Ptr | Ty::Unit | Ty::V128 | Ty::Agg(_) | Ty::Bool),
    };
    let each = sig.params.iter().zip(params).all(|(p, &t)| match p.mode {
        Mode::Borrow => t == Ty::Ptr,
        _ => scalar(&p.ty, t),
    });
    let result = match sig.ret {
        T::Str => params.last() == Some(&Ty::Ptr) && ret == Ty::Unit,
        T::Void => ret == Ty::Unit,
        _ => scalar(&sig.ret, ret),
    };
    each && result
}

/// The ids of the state declared in `private` (the module's private modules): its constants'
/// thread-local values and static atomics, named as lowering names them (`velt_vir`
/// `tls::stable_id` of the qualified name and value type) for each type a value can have.
fn own_state(hir: &velt_sema::hir::Program, private: &[&str]) -> HashSet<u64> {
    use velt_sema::hir::Def;
    const WORDS: [Ty; 11] = [
        Ty::I8,
        Ty::I16,
        Ty::I32,
        Ty::I64,
        Ty::U8,
        Ty::U16,
        Ty::U32,
        Ty::U64,
        Ty::F32,
        Ty::F64,
        Ty::Bool,
    ];
    let mut own = HashSet::new();
    for d in &hir.defs {
        let Def::Global(g) = d else { continue };
        let Some((module, _)) = g.name.rsplit_once("::") else {
            continue;
        };
        if private.contains(&module) {
            for vt in WORDS {
                own.insert(velt_vir::tls::stable_id(&format!("{}:{vt:?}", g.name)));
            }
        }
    }
    own
}

/// `Err` where the object's code (its optimized functions with bodies, and the statics they name)
/// holds state a program shares with it (module docs); `own` is the module's own ([`own_state`]).
fn shares_nothing(program: &vir::Program, own: &HashSet<u64>) -> Result<(), String> {
    let (externs, statics) = reached(program);
    let shared = ["velt_rt_module_state", "velt_tls_", "velt_rt_statics"];
    let owned = |symbol: &str| {
        velt_vir::tls::TlsCall::of_symbol(symbol).is_some_and(|c| own.contains(&c.id()))
    };
    if let Some(e) = (externs.iter().map(|&e| &program.externs[e]))
        .find(|e| shared.iter().any(|s| e.symbol.starts_with(s)) && !owned(&e.symbol))
    {
        return Err(format!("its code reaches `{}`", e.symbol));
    }
    let shared_static = |s: &vir::StaticData| {
        s.linkage != Linkage::Internal || s.mutable.is_some_and(|id| !own.contains(&id))
    };
    if statics.iter().any(|&s| shared_static(&program.statics[s])) {
        return Err("its code reaches a static atomic or a named static".into());
    }
    Ok(())
}

/// The module that keeps the blocks of module state (std/lang/hooks/module_blocks.velt): its
/// canonical path, and what its functions' symbols hold of it (its name, mangled).
const BLOCKS_MODULE: &str = "std/lang/hooks/module_blocks";
const BLOCKS_CODE: &str = "module__blocks";

/// `Err` where the object's code is module state's own, or reaches module state: the executor's
/// module hooks (its lock), module_blocks.velt's functions (the blocks and their table, which a
/// copy in the object would split in two), or a call of that module's entry points
/// (`velt_hook_module_*`, whose object only a program that reaches module state itself links).
/// The exception is the object of module_blocks.velt itself (`private` names it, `prebuilt.rs`
/// "State"): the blocks are its own state, which every program reaches through its entry points
/// alone.
fn holds_no_module_state(program: &vir::Program, private: &[&str]) -> Result<(), String> {
    let own = private.contains(&BLOCKS_MODULE);
    let module_code = |s: &str| s.contains("schedModule") || (!own && s.contains(BLOCKS_CODE));
    if let Some(f) = (program.funcs.iter()).find(|f| module_code(&f.symbol) && !f.blocks.is_empty())
    {
        return Err(format!("its code reaches module state (`{}`)", f.symbol));
    }
    let entry = format!("{}module_", prebuilt::PREFIX);
    let (externs, _) = reached(program);
    if let Some(e) =
        (externs.iter().map(|&e| &program.externs[e])).find(|e| e.symbol.starts_with(&entry))
    {
        return Err(format!("its code reaches module state (`{}`)", e.symbol));
    }
    Ok(())
}

/// The `velt_tls_*` externs the code of `program` reaches, in their canonical spelling.
fn thread_locals(program: &vir::Program) -> Vec<String> {
    let (externs, _) = reached(program);
    let mut tls: Vec<String> = (externs.iter())
        .filter_map(|&e| velt_vir::tls::TlsCall::of_symbol(&program.externs[e].symbol))
        .map(|c| c.symbol())
        .collect();
    tls.sort();
    tls.dedup();
    tls
}

/// The externs and statics the bodies of `program`'s functions name, directly or through the
/// statics' relocations (the lists keep entries nothing names any more).
fn reached(program: &vir::Program) -> (Vec<usize>, Vec<usize>) {
    let mut externs = std::collections::BTreeSet::new();
    let mut statics = std::collections::BTreeSet::new();
    let mut pending = vec![];
    for f in program.funcs.iter().filter(|f| !f.blocks.is_empty()) {
        super::select::visit(f, &mut |op| match op {
            Operand::Const(Const::Extern(e), _) => {
                externs.insert(e.0 as usize);
            }
            Operand::Const(Const::Static(s), _) => pending.push(s.0 as usize),
            _ => {}
        });
        for b in &f.blocks {
            if let Terminator::Call {
                callee: Callee::Extern(e),
                ..
            } = &b.term
            {
                externs.insert(e.0 as usize);
            }
        }
    }
    while let Some(s) = pending.pop() {
        if !statics.insert(s) {
            continue;
        }
        for (_, target) in &program.statics[s].relocs {
            match target {
                Const::Static(t) => pending.push(t.0 as usize),
                Const::Extern(e) => {
                    externs.insert(e.0 as usize);
                }
                _ => {}
            }
        }
    }
    (externs.into_iter().collect(), statics.into_iter().collect())
}

#[cfg(test)]
#[path = "prebuilt_object_tests.rs"]
mod tests;

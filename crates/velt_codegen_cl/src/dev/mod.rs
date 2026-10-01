//! In-process JIT for `velt dev --host`, with hot swap (docs/internals/design/hot-reload.md, phases 2–3).
//!
//! A [`DevSession`] compiles VIR programs with Cranelift straight into executable memory,
//! through the same `build_module` as object files, so dev builds need no linker and no new
//! executable. Each version is its own `JITModule`, and loaded code is never freed during a
//! session: futures in flight keep running the code of the version that created them.
//!
//! - Every swappable function has a slot and a trampoline (`trampoline`): calls and function
//!   addresses go through the trampoline, so a swap is one atomic store per changed function.
//!   Pinned functions (`roles`: async `$poll`/`$drop`, handler `$init`, …) are referenced
//!   directly, by the code of their own version.
//! - [`DevSession::reload`] compares the new version with the running one per function key
//!   (`facts`, `fingerprint`, `classify`): either it compiles the changed functions into a new
//!   module and swaps them in, or it reports why the program must restart.
//! - On Windows x64 each version's unwind info is registered with the system
//!   (`unwind::jit_windows`), so stack walks get through JIT frames; elsewhere JIT code has no
//!   registered unwind info yet.
//!
//! Runtime functions resolve through the symbol table the host passes in (`velt_rt`'s
//! `ABI_SYMBOLS`); anything else (`memcpy`, `fmod`, ...) through the process's dynamic symbols.

mod classify;
mod facts;
mod fingerprint;
mod handlers;
mod roles;
mod trampoline;
mod version;

use std::collections::{HashMap, HashSet};

use cranelift_jit::JITModule;
use velt_vir::vir;

pub use handlers::HandlerCode;

use crate::module::DevFunction;
use classify::Decision;
use facts::Facts;
use trampoline::Slot;

/// Swaps after which the host restarts to reclaim the memory of old versions.
const MAX_SWAPS: u32 = 200;

/// A JIT session: owns every version it loaded (and so the code's memory), the slots and
/// trampolines, and the facts of the running version.
pub struct DevSession {
    runtime: Vec<(String, usize)>,
    versions: Vec<JITModule>,
    /// Swappable functions by key: slot and trampoline address.
    slots: HashMap<String, (Slot, usize)>,
    /// Pinned functions by key: their newest code.
    pinned: HashMap<String, usize>,
    /// Pinned code of every version → its key (to find a running handler's newer code).
    owners: HashMap<usize, String>,
    /// Newest state size and alignment per handler `$init` key.
    handler_states: HashMap<String, (u64, u64)>,
    running: Option<Facts>,
    swaps: u32,
}

/// A program loaded by [`DevSession::load`]; valid as long as its session lives.
#[derive(Clone, Copy, Debug)]
pub struct JitProgram {
    main: usize,
}

/// What [`DevSession::reload`] did with a new version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reload {
    /// The changed functions were swapped in: new calls run them.
    Swapped {
        /// How many functions were compiled and swapped in.
        functions: usize,
    },
    /// The program must restart to run the new version (nothing was changed); the reason.
    Restart(String),
}

impl DevSession {
    /// A session resolving `symbols` (name → address) for the generated code.
    pub fn new(symbols: &[(&str, *const u8)]) -> Self {
        DevSession {
            runtime: symbols
                .iter()
                .map(|(n, p)| (n.to_string(), *p as usize))
                .collect(),
            versions: vec![],
            slots: HashMap::new(),
            pinned: HashMap::new(),
            owners: HashMap::new(),
            handler_states: HashMap::new(),
            running: None,
            swaps: 0,
        }
    }

    /// Compile all of `program` for the host and make it the running version. The program must
    /// define `velt_main`. Compiled without Cranelift optimizations (fast compiles for the dev
    /// loop).
    pub fn load(&mut self, program: &vir::Program) -> Result<JitProgram, String> {
        verify(program)?;
        let main_index = program
            .funcs
            .iter()
            .position(|f| f.symbol == "velt_main")
            .ok_or("codegen: the program has no `velt_main`")?;
        let all: HashSet<&str> = program.funcs.iter().map(|f| f.symbol.as_str()).collect();
        let code = self.install(program, &all, true)?;
        self.running = Some(Facts::of(program));
        let main = code[main_index].ok_or("ICE: `velt_main` was not compiled")?;
        Ok(JitProgram { main })
    }

    /// Bring the running program up to `program`: swap in its changed functions, or report why
    /// it needs a restart (the running code is then left as it is).
    pub fn reload(&mut self, program: &vir::Program) -> Result<Reload, String> {
        verify(program)?;
        let Some(running) = &self.running else {
            return Err("ICE: reload before load".into());
        };
        if self.swaps >= MAX_SWAPS {
            let reason = format!("reclaiming the memory of {MAX_SWAPS} hot swaps");
            return Ok(Reload::Restart(reason));
        }
        let facts = Facts::of(program);
        match classify::classify(running, &facts) {
            Decision::Restart(reason) => Ok(Reload::Restart(reason)),
            Decision::Swap(keys) => {
                // Nothing compiled (e.g. only comments changed): no new module either.
                if !keys.is_empty() {
                    let define: HashSet<&str> = keys.iter().map(String::as_str).collect();
                    self.install(program, &define, false)?;
                    self.swaps += 1;
                }
                self.running = Some(facts);
                Ok(Reload::Swapped {
                    functions: keys.len(),
                })
            }
        }
    }

    /// The newest code for the HTTP handler whose descriptor has `init` as its init function,
    /// if a swap recompiled it since.
    pub fn handler_update(&self, init: usize) -> Option<HandlerCode> {
        let key = self.owners.get(&init)?;
        let newest = *self.pinned.get(key)?;
        if newest == init {
            return None;
        }
        let (poll, drop) = handlers::state_machine_keys(key)?;
        let (state_size, state_align) = *self.handler_states.get(key)?;
        Some(HandlerCode {
            init: newest,
            poll: *self.pinned.get(&poll)?,
            drop: *self.pinned.get(&drop)?,
            state_size,
            state_align,
        })
    }

    /// Compile the functions of `program` in `define` as a new version and switch to them.
    /// Returns the code address of each defined function.
    fn install(
        &mut self,
        program: &vir::Program,
        define: &HashSet<&str>,
        first: bool,
    ) -> Result<Vec<Option<usize>>, String> {
        let (names, new_slots) = self.names(program, define);
        let imports = self.imports(program, &names)?;
        let version = version::compile(program, &names, &imports, &self.runtime, first)?;
        for (i, func) in program.funcs.iter().enumerate() {
            let Some(code) = version.code[i] else {
                continue;
            };
            if roles::is_pinned(&func.symbol) {
                self.pinned.insert(func.symbol.clone(), code);
                self.owners.insert(code, func.symbol.clone());
                if roles::is_handler_init(&func.symbol) {
                    if let Some(layout) = handlers::state_layout(func, program) {
                        self.handler_states.insert(func.symbol.clone(), layout);
                    }
                }
            }
        }
        let mut new_slots = new_slots.into_iter();
        for (func, trampoline) in program.funcs.iter().zip(&version.trampolines) {
            if let Some(trampoline) = trampoline {
                let slot = new_slots.next().ok_or("ICE: a trampoline without a slot")?;
                self.slots.insert(func.symbol.clone(), (slot, *trampoline));
            }
        }
        // Slots last: from here on, calls reach the new code.
        for (i, func) in program.funcs.iter().enumerate() {
            if let (Some(code), Some((slot, _))) = (version.code[i], self.slots.get(&func.symbol)) {
                slot.set(code as *const u8);
            }
        }
        let code = version.code.clone();
        self.versions.push(version.module);
        Ok(code)
    }

    /// How the version declares each function, plus the slots of the trampolines it defines
    /// (in function order).
    fn names(
        &self,
        program: &vir::Program,
        define: &HashSet<&str>,
    ) -> (Vec<DevFunction>, Vec<Slot>) {
        let mut slots = vec![];
        let names = program
            .funcs
            .iter()
            .map(|func| {
                let key = &func.symbol;
                let defined = define.contains(key.as_str());
                if roles::is_pinned(key) {
                    return DevFunction {
                        reference: key.clone(),
                        trampoline: None,
                        code: defined.then(|| key.clone()),
                    };
                }
                let trampoline = (!self.slots.contains_key(key)).then(|| {
                    let slot = Slot::new();
                    let code = slot.trampoline();
                    slots.push(slot);
                    code
                });
                DevFunction {
                    reference: key.clone(),
                    trampoline,
                    code: defined.then(|| format!("{key}$impl")),
                }
            })
            .collect();
        (names, slots)
    }

    /// Addresses of the references a version does not define: trampolines of existing keys and
    /// the newest code of pinned functions.
    fn imports(
        &self,
        program: &vir::Program,
        names: &[DevFunction],
    ) -> Result<Vec<(String, usize)>, String> {
        let mut imports = vec![];
        for (func, name) in program.funcs.iter().zip(names) {
            let local = name.trampoline.is_some() || name.code.as_ref() == Some(&name.reference);
            if local {
                continue;
            }
            let address = if roles::is_pinned(&func.symbol) {
                self.pinned.get(&func.symbol).copied()
            } else {
                self.slots.get(&func.symbol).map(|(_, t)| *t)
            };
            let address =
                address.ok_or_else(|| format!("ICE: `{}` has no code to refer to", func.symbol))?;
            imports.push((name.reference.clone(), address));
        }
        Ok(imports)
    }
}

impl JitProgram {
    /// The program's `int32_t velt_main(void)`.
    pub fn main(&self) -> extern "C" fn() -> i32 {
        // SAFETY: `velt_main` is compiled with the C ABI signature `() -> i32` (rt_abi.md), and
        // the code stays mapped for the session's lifetime.
        unsafe { std::mem::transmute::<usize, extern "C" fn() -> i32>(self.main) }
    }
}

fn verify(program: &vir::Program) -> Result<(), String> {
    velt_vir::verify(program).map_err(|errs| format!("invalid VIR:\n  {}", errs.join("\n  ")))
}

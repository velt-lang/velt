//! Debug runtime only: which task owns each captured variable's cell (#916).
//!
//! A variable that a closure assigns while something else still sees it lives in a counted heap
//! cell (`LocalDef::boxed`, velt_vir `lower/cells.rs`). Its count is plain, so the cell must
//! never be used by two tasks: a value crossing to another task gets a cell of its own
//! (transfer glue, `own_cell`), and sema rejects the shapes that would share one (an HTTP
//! handler reaching a closure that assigns a captured variable, #901). If something slipped
//! through, requests on several threads would update the count (or the value) at the same
//! time and crash now and then; this check makes it fail at once, with a message.
//!
//! Debug programs built by a debug compiler call these functions (velt_vir
//! `LowerOptions::cell_checks`); a side table keyed by the cell's address records the owning
//! *task* (`velt_rt_task_id`), not the OS thread: a task (with its local promises) resumes on
//! whichever worker is free, and that is not a race, since only one thread polls a task at a
//! time. The rules:
//! - `cell_new` records the current task as the owner (none outside a task: unowned);
//! - `cell_use` (a count change, or the call of a closure that assigns the variable) from a task
//!   claims an unowned cell and aborts if another task owns it; outside any task (runtime
//!   clean-up such as dropping a cancelled task's state, an HTTP request's set-up) nothing is
//!   checked or claimed;
//! - `cell_give` (the transfer glue moving the only reference to another task) and
//!   `cell_copy` (the transfer's fresh copy) leave the cell unowned: the receiving task claims
//!   it on first use;
//! - `cell_free` forgets the cell (a new cell at the same address is recorded anew anyway).
//!
//! The release runtime keeps the symbols as empty functions (release programs never call them),
//! so the cell layout and the generated code of release builds are unchanged.

use crate::str::VeltStr;

#[cfg(debug_assertions)]
mod table {
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub(super) struct Entry {
        /// Owning task (0: unowned, the next task to use it claims it).
        pub owner: u64,
        /// The thread the owner last claimed it on, for the message.
        pub thread: String,
        pub name: String,
    }

    pub(super) static CELLS: Mutex<Option<HashMap<usize, Entry>>> = Mutex::new(None);

    pub(super) fn with<R>(f: impl FnOnce(&mut HashMap<usize, Entry>) -> R) -> R {
        let mut g = CELLS.lock().unwrap_or_else(|e| e.into_inner());
        f(g.get_or_insert_with(HashMap::new))
    }

    pub(super) fn thread() -> String {
        let t = std::thread::current();
        match t.name() {
            Some(n) => format!("thread `{n}`"),
            None => format!("{:?}", t.id()),
        }
    }

    pub(super) fn task() -> u64 {
        crate::task::local::velt_rt_task_id()
    }
}

/// The variable's name as lowering passed it (a string literal), or `?`.
#[cfg(debug_assertions)]
unsafe fn name_of(name: *const VeltStr) -> String {
    match name.as_ref() {
        Some(s) => String::from_utf8_lossy(s.as_bytes()).into_owned(),
        None => "?".into(),
    }
}

/// A new cell at `cell`, holding variable `name`, owned by the task making it.
///
/// # Safety
/// `name` is null or points to a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_cell_new(cell: *const u8, name: *const VeltStr) {
    #[cfg(debug_assertions)]
    {
        let name = name_of(name);
        let owner = table::task();
        let thread = table::thread();
        table::with(|t| {
            t.insert(
                cell as usize,
                table::Entry {
                    owner,
                    thread,
                    name,
                },
            )
        });
    }
    #[cfg(not(debug_assertions))]
    let _ = (cell, name);
}

/// The task being polled uses `cell` (changes its count, or calls a closure assigning it).
#[no_mangle]
pub extern "C" fn velt_rt_cell_use(cell: *const u8) {
    #[cfg(debug_assertions)]
    {
        let me = table::task();
        if me == 0 {
            return;
        }
        let clash = table::with(|t| {
            let e = t.entry(cell as usize).or_insert_with(|| table::Entry {
                owner: 0,
                thread: String::new(),
                name: "?".into(),
            });
            if e.owner == 0 {
                e.owner = me;
                e.thread = table::thread();
                None
            } else if e.owner != me {
                Some((e.name.clone(), e.owner, e.thread.clone()))
            } else {
                None
            }
        });
        if let Some((name, owner, thread)) = clash {
            report(&name, owner, &thread, me);
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = cell;
}

/// The only reference to `cell` moves to another task (transfer glue): it is unowned until the
/// receiving task uses it.
#[no_mangle]
pub extern "C" fn velt_rt_cell_give(cell: *const u8) {
    #[cfg(debug_assertions)]
    table::with(|t| {
        if let Some(e) = t.get_mut(&(cell as usize)) {
            e.owner = 0;
        }
    });
    #[cfg(not(debug_assertions))]
    let _ = cell;
}

/// `cell` is a fresh copy of `from` made for another task (transfer glue): unowned until the
/// receiving task uses it, holding the same variable.
#[no_mangle]
pub extern "C" fn velt_rt_cell_copy(cell: *const u8, from: *const u8) {
    #[cfg(debug_assertions)]
    table::with(|t| {
        let name = t
            .get(&(from as usize))
            .map_or_else(|| "?".into(), |e| e.name.clone());
        t.insert(
            cell as usize,
            table::Entry {
                owner: 0,
                thread: String::new(),
                name,
            },
        );
    });
    #[cfg(not(debug_assertions))]
    let _ = (cell, from);
}

/// `cell` is about to be freed (its last reference was released, which `cell_use` checked).
#[no_mangle]
pub extern "C" fn velt_rt_cell_free(cell: *const u8) {
    #[cfg(debug_assertions)]
    table::with(|t| t.remove(&(cell as usize)));
    #[cfg(not(debug_assertions))]
    let _ = cell;
}

#[cfg(debug_assertions)]
#[cold]
fn report(name: &str, owner: u64, owner_thread: &str, me: u64) -> ! {
    use std::io::Write;
    crate::io::try_flush_stdout();
    let here = table::thread();
    let var = if name == "?" {
        "a captured variable".to_string()
    } else {
        format!("captured variable `{name}`")
    };
    let msg = format!(
        "velt debug-cells: {var} was used by two tasks: it belongs to task {owner} (on {owner_thread}), \
         and task {me} (on {here}) used it too. A closure that assigns a variable it captured was \
         shared with another task or HTTP request instead of copied, so both would change the \
         variable (and its reference count) at the same time. Keep one value every task shares \
         in `shared(...)` instead (a `shared(new Mutex(...))` for anything but a 64-bit integer).\n"
    );
    let _ = std::io::stderr().write_all(msg.as_bytes());
    std::process::abort()
}

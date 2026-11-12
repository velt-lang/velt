//! The debug runtime's owner check of captured variables' cells (`cell_owner.rs`, #916): a cell
//! belongs to one task, whichever threads that task runs on; another task using it aborts, while
//! a cell handed over by the transfer glue is claimed by the task that receives it.
//!
//! Tasks are played by [`Locals`], which every task root (spawned tasks, `block_on`, HTTP
//! requests) polls through and which gives `velt_rt_task_id` its id.

use crate::cell_owner::{
    velt_rt_cell_copy, velt_rt_cell_free, velt_rt_cell_give, velt_rt_cell_new, velt_rt_cell_use,
};
use crate::str::VeltStr;
use crate::task::local::{velt_rt_task_id, Locals};
use std::task::{Context, Poll, Waker};

/// A task: `f` runs inside one poll of it.
struct Task(Locals);

impl Task {
    fn new() -> Task {
        Task(Locals::default())
    }

    fn poll(&mut self, f: impl FnOnce()) {
        let mut cx = Context::from_waker(Waker::noop());
        let mut f = Some(f);
        let _ = self.0.poll_root(&mut cx, |_| {
            if let Some(f) = f.take() {
                f();
            }
            Poll::Pending
        });
    }
}

/// A fresh "cell" address (any distinct address will do; the check only keys on it), as an
/// integer so closures moving to other threads can hold it.
fn cell() -> usize {
    Box::leak(Box::new(0u64)) as *const u64 as usize
}

fn new(c: usize) {
    let name = VeltStr::from_static(b"last");
    // SAFETY: `name` is a valid string.
    unsafe { velt_rt_cell_new(c as *const u8, &name) }
}

fn use_(c: usize) {
    velt_rt_cell_use(c as *const u8);
}

#[test]
fn a_task_keeps_its_cells_when_it_resumes_on_another_thread() {
    let c = cell();
    let mut t = Task::new();
    t.poll(|| new(c));
    let t = std::thread::spawn(move || {
        t.poll(|| {
            assert_ne!(velt_rt_task_id(), 0);
            use_(c);
            use_(c);
        });
        t
    })
    .join()
    .unwrap();
    let mut t = t;
    t.poll(|| {
        use_(c);
        velt_rt_cell_free(c as *const u8);
    });
}

#[test]
fn uses_outside_a_task_are_not_checked() {
    let c = cell();
    let mut t = Task::new();
    t.poll(|| new(c));
    // Runtime clean-up (a cancelled task's state, a request's set-up) runs outside any task.
    use_(c);
    std::thread::scope(|s| {
        s.spawn(|| use_(c));
    });
    t.poll(|| use_(c));
}

#[test]
fn a_cell_made_outside_a_task_is_claimed_by_the_first_task_using_it() {
    let c = cell();
    new(c);
    let mut t = Task::new();
    t.poll(|| use_(c));
    t.poll(|| use_(c));
}

#[test]
fn a_cell_moved_to_another_task_belongs_to_it() {
    let (moved, copied) = (cell(), cell());
    let (mut a, mut b) = (Task::new(), Task::new());
    a.poll(|| {
        new(moved);
        new(copied);
        use_(moved);
        // The transfer glue: the only reference moves; a shared cell is copied.
        velt_rt_cell_give(moved as *const u8);
    });
    let copy = cell();
    a.poll(|| velt_rt_cell_copy(copy as *const u8, copied as *const u8));
    std::thread::scope(|s| {
        s.spawn(|| {
            b.poll(|| {
                use_(moved);
                use_(copy);
            })
        });
    });
    b.poll(|| {
        use_(moved);
        velt_rt_cell_free(moved as *const u8);
        velt_rt_cell_free(copy as *const u8);
    });
    a.poll(|| use_(copied));
}

/// Set in the child process of [`a_cell_used_by_two_tasks_aborts`].
const TWO_TASKS_CHILD: &str = "VELT_RT_CELL_TWO_TASKS_CHILD";

/// #916: per-request copies of a closure that assigns a captured variable share its cell; the
/// first request task touching it aborts, deterministically, without any load.
#[test]
fn a_cell_used_by_two_tasks_aborts() {
    if std::env::var_os(TWO_TASKS_CHILD).is_some() {
        let c = cell();
        let mut main = Task::new();
        main.poll(|| new(c));
        // A request, on a worker thread: copying the closure retains the cell.
        std::thread::spawn(move || Task::new().poll(|| use_(c)))
            .join()
            .unwrap();
        unreachable!("the second task's use must abort");
    }
    let exe = std::env::current_exe().expect("test executable");
    let out = super::command::command(exe)
        .args([
            "--exact",
            "abi_tests::cells::a_cell_used_by_two_tasks_aborts",
        ])
        .env(TWO_TASKS_CHILD, "1")
        .output()
        .expect("run the child");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the child must abort: {stderr}");
    assert!(
        stderr.contains("velt debug-cells: captured variable `last` was used by two tasks"),
        "{stderr}"
    );
    assert!(stderr.contains("shared(...)"), "{stderr}");
}

/// Set in the child processes of the module-state tests: they take the module lock, which is
/// process-wide.
const MODULE_CHILD: &str = "VELT_RT_CELL_MODULE_CHILD";

/// Runs test `name` in a child process with [`MODULE_CHILD`] set; its output.
fn module_child(name: &str) -> std::process::Output {
    let exe = std::env::current_exe().expect("test executable");
    super::command::command(exe)
        .args(["--exact", &format!("abi_tests::cells::{name}")])
        .env(MODULE_CHILD, "1")
        .output()
        .expect("run the child")
}

/// #2737: a closure stored in module state is called by every task that uses module state; they
/// hold the module lock through each poll, as does the task that stored it, so they share its
/// cells without overlapping.
#[test]
fn tasks_holding_the_module_lock_share_a_cell() {
    if std::env::var_os(MODULE_CHILD).is_none() {
        let out = module_child("tasks_holding_the_module_lock_share_a_cell");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "the child must not abort: {stderr}");
        return;
    }
    let c = cell();
    let mut main = Task::new();
    main.poll(|| {
        new(c);
        // Storing the closure in module state.
        crate::task::local::module_touch();
        use_(c);
    });
    let others = std::thread::spawn(move || {
        let mut t = Task::new();
        t.poll(|| {
            crate::task::local::module_touch();
            use_(c);
        });
        // A later poll of a task that touched module state takes the lock at its start.
        t.poll(|| use_(c));
    });
    others.join().unwrap();
    main.poll(|| use_(c));
}

/// A cell shared through module state used by a task that does not hold the module lock aborts.
#[test]
fn a_cell_shared_through_module_state_used_without_the_lock_aborts() {
    if std::env::var_os(MODULE_CHILD).is_some() {
        let c = cell();
        let mut main = Task::new();
        main.poll(|| {
            new(c);
            crate::task::local::module_touch();
        });
        std::thread::spawn(move || {
            Task::new().poll(|| {
                crate::task::local::module_touch();
                use_(c);
            })
        })
        .join()
        .unwrap();
        // A task that never used module state.
        std::thread::spawn(move || Task::new().poll(|| use_(c)))
            .join()
            .unwrap();
        unreachable!("the use without the module lock must abort");
    }
    let out = module_child("a_cell_shared_through_module_state_used_without_the_lock_aborts");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the child must abort: {stderr}");
    assert!(
        stderr.contains("captured variable `last` is shared through module state"),
        "{stderr}"
    );
}

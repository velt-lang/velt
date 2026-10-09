//! Emulated async runtime (rt_abi_async.md §1–§2, §8–§9) for the interpreter: a deterministic
//! single-threaded executor with a virtual clock. `block_on` polls the root future, then every
//! unfinished task, round after round; when a round made no yield-style progress the clock jumps
//! to the earliest sleep deadline. Heap futures live in interpreter memory (header 16 bytes,
//! result/state at +16) so leak checks cover them; their behavior is kept in `futs`.

use std::collections::HashMap;

use super::{Interp, FUNC_BASE};

/// Opaque `cx` passed to poll functions (never dereferenced).
pub(super) const CX: u64 = 0xC0DE;
const MAX_ROUNDS: u32 = 1_000_000;

/// What a heap future does when polled.
pub(super) enum Fut {
    /// `velt_rt_fut_box`: compiled state at +16.
    Boxed {
        poll: usize,
        drop: usize,
        done: bool,
    },
    Sleep {
        deadline: f64,
    },
    Yield {
        polled: bool,
    },
    /// `velt_rt_all[_with_drop]`: pending children (0 once finished) and the result drop
    /// function address (0 = none), run for finished results if cancelled before completing.
    All {
        children: Vec<u64>,
        results: u64,
        size: u64,
        result_drop: u64,
        complete: bool,
        /// `velt_rt_all_or_reject`: complete at the first `Err` result, moved to slot 0 (which
        /// then belongs to the awaiter); `rejected` is the child it came from.
        reject_early: bool,
        rejected: Option<usize>,
    },
    /// Join handle of task `task`; result of `size` bytes.
    Join {
        task: usize,
        size: u64,
    },
    /// Completes on the first poll (its result was written at creation).
    Ready,
    /// A boxed state started by `velt_rt_fut_start` (local.rs): driven every round.
    Started {
        poll: usize,
        result_drop: u64,
        done: bool,
        delivered: bool,
        detached: bool,
    },
    /// `velt_rt_race`: children still racing (the winner's `size`-byte result goes to +16).
    /// `first_ok` (`velt_rt_race_ok`): rejected results lose while other children run; the
    /// address of their drop function (0 = none).
    Race {
        children: Vec<u64>,
        size: u64,
        first_ok: Option<u64>,
    },
    /// Emulated socket/file operations that may have to wait (net.rs).
    Io(super::net::IoOp),
}

/// A spawned task: its future, and its result bytes once finished.
struct Task {
    fut: u64,
    size: u64,
    result: Option<Vec<u8>>,
}

#[derive(Default)]
pub(super) struct Exec {
    pub futs: HashMap<u64, Fut>,
    tasks: Vec<Task>,
    /// Inside `block_on`: promises are started only while tasks run (local.rs).
    pub running: bool,
    /// Started, unfinished promises (local.rs).
    pub locals: Vec<u64>,
    pub now: f64,
    /// Something asked to be polled again without time passing (yield, I/O readiness).
    pub woke: bool,
    pub net: super::net::Net,
    pub json: super::json::Readers,
    pub http: super::http::Http,
}

impl Interp<'_> {
    pub(super) fn func_id(&self, p: u64) -> usize {
        p.checked_sub(FUNC_BASE)
            .expect("interp: expected a function pointer") as usize
    }

    /// A new heap future object with `size` result/state bytes.
    pub(super) fn new_fut(&mut self, size: u64, kind: Fut) -> u64 {
        let f = self.heap_alloc(16 + size);
        self.exec.futs.insert(f, kind);
        f
    }

    /// M3/M4 runtime functions (futures, tasks, shared state, I/O, string builder, JSON
    /// reader); `None` if `sym` is not one of them.
    pub(super) fn rt_async(&mut self, sym: &str, a: &[u64]) -> Option<Result<u64, i32>> {
        Some(Ok(match sym {
            "velt_rt_fut_poll" => return Some(self.fut_poll(a[0])),
            "velt_rt_fut_drop" => return Some(self.fut_drop(a[0]).map(|_| 0)),
            "velt_rt_fut_box" => {
                let (poll, drop) = (self.func_id(a[0]), self.func_id(a[1]));
                assert!(a[4] <= 16, "interp: state alignment {}", a[4]);
                let kind = Fut::Boxed {
                    poll,
                    drop,
                    done: false,
                };
                let f = self.new_fut(a[3], kind);
                let b = self.read_bytes(a[2], a[3] as usize);
                self.write_bytes(f + 16, &b);
                f
            }
            "velt_rt_fut_start" => return Some(self.fut_start(a[0], a[1])),
            "velt_rt_fut_detach" => return Some(self.fut_detach(a[0], a[1]).map(|_| 0)),
            // One thread: a result never crosses to another one.
            "velt_rt_fut_transfer" => 0,
            // Transfers copy as if each object were reached once (velt_rt's transfer_map is not
            // emulated): `find` answers "no transfer under way", `defer` "release it now".
            "velt_rt_xfer_begin" | "velt_rt_xfer_end" | "velt_rt_xfer_record" => 0,
            "velt_rt_xfer_suspend" | "velt_rt_xfer_resume" => 0,
            "velt_rt_xfer_find" => 1,
            "velt_rt_xfer_defer" => 0,
            // One thread: copies never take turns, and no handler copies its captures.
            "velt_rt_copy_lock" | "velt_rt_copy_unlock" | "velt_rt_saw_cells" => 0,
            "velt_rt_take_cells" => 0,
            "velt_rt_futs_handled" => return Some(self.futs_handled(a[0], a[1], a[2]).map(|_| 0)),
            "velt_rt_race" => self.rt_race(a, None),
            "velt_rt_race_ok" => self.rt_race(a, Some(a[3])),
            "velt_rt_yield_now" => {
                assert_eq!(a[0], CX, "interp: yield_now needs the poll's cx");
                self.exec.woke = true;
                0
            }
            "velt_rt_yield_now_fut" => self.new_fut(0, Fut::Yield { polled: false }),
            "velt_rt_sleep" => {
                let deadline = self.exec.now + (a[0] as i64).max(0) as f64;
                self.new_fut(0, Fut::Sleep { deadline })
            }
            "velt_rt_all" | "velt_rt_all_with_drop" => self.rt_all(a, false),
            "velt_rt_all_or_reject" => self.rt_all(a, true),
            "velt_rt_block_on" => return Some(self.block_on(a[0], a[1]).map(|_| 0)),
            // Single-threaded: the result needs no transfer.
            "velt_rt_spawn" | "velt_rt_spawn_transfer" => {
                let (poll, drop) = (self.func_id(a[0]), self.func_id(a[1]));
                let kind = Fut::Boxed {
                    poll,
                    drop,
                    done: false,
                };
                let f = self.new_fut(a[3], kind);
                let b = self.read_bytes(a[2], a[3] as usize);
                self.write_bytes(f + 16, &b);
                self.spawn_task(f, a[5])
            }
            "velt_rt_spawn_fut" => self.spawn_task(a[0], a[1]),
            "velt_rt_perf_now" => self.exec.now.to_bits(),
            "velt_rt_date_now" => 1_700_000_000_000u64 + self.exec.now as u64,
            _ => {
                return self
                    .rt_sync_shared(sym, a)
                    .or_else(|| self.rt_net(sym, a))
                    .or_else(|| self.rt_http(sym, a))
                    .or_else(|| self.rt_strbuf(sym, a).map(Ok))
                    .or_else(|| self.rt_json(sym, a).map(Ok))
            }
        }))
    }

    /// Atomics and mutexes (single-threaded: a second lock is a deadlock bug).
    fn rt_sync_shared(&mut self, sym: &str, a: &[u64]) -> Option<Result<u64, i32>> {
        Some(Ok(match sym {
            "velt_rt_atomic_add_i64" => {
                let v = (self.read_u64(a[0]) as i64).wrapping_add(a[1] as i64) as u64;
                self.write_bytes(a[0], &v.to_le_bytes());
                v
            }
            "velt_rt_atomic_load_i64" => self.read_u64(a[0]),
            "velt_rt_atomic_store_i64" => {
                self.write_bytes(a[0], &a[1].to_le_bytes());
                0
            }
            "velt_rt_mutex_init" => {
                self.write_bytes(a[0], &0u64.to_le_bytes());
                0
            }
            "velt_rt_mutex_lock" => {
                assert_eq!(self.read_u64(a[0]), 0, "interp: mutex already locked");
                self.write_bytes(a[0], &1u64.to_le_bytes());
                0
            }
            "velt_rt_mutex_unlock" => {
                assert_eq!(
                    self.read_u64(a[0]),
                    1,
                    "interp: unlock of an unlocked mutex"
                );
                self.write_bytes(a[0], &0u64.to_le_bytes());
                0
            }
            _ => return None,
        }))
    }

    fn rt_all(&mut self, a: &[u64], reject_early: bool) -> u64 {
        let children = (0..a[1]).map(|i| self.read_u64(a[0] + 8 * i)).collect();
        let kind = Fut::All {
            children,
            results: a[3],
            size: a[2],
            result_drop: a.get(4).copied().unwrap_or(0),
            complete: false,
            reject_early,
            rejected: None,
        };
        self.new_fut(0, kind)
    }

    /// Call a function address (compiled function or rt extern) with scalar arguments.
    pub(super) fn call_addr(&mut self, target: u64, args: Vec<u64>) -> Result<u64, i32> {
        if let Some(e) = target.checked_sub(super::EXTERN_BASE) {
            let sym = self.p.externs[e as usize].symbol.clone();
            return self.rt(&sym, &args);
        }
        let argv = args.into_iter().map(super::V::S).collect();
        Ok(self.call_fn(self.func_id(target), argv)?.s())
    }

    fn spawn_task(&mut self, fut: u64, size: u64) -> u64 {
        let task = self.start_task(fut, size);
        self.new_fut(size, Fut::Join { task, size })
    }

    /// Run heap future `fut` as a task with a `size`-byte result; returns the task index.
    pub(super) fn start_task(&mut self, fut: u64, size: u64) -> usize {
        assert!(size <= 256, "interp: task result of {size} bytes");
        self.exec.tasks.push(Task {
            fut,
            size,
            result: None,
        });
        self.exec.tasks.len() - 1
    }

    /// Result bytes of task `t`, once it finished.
    pub(super) fn task_result(&self, t: usize) -> Option<Vec<u8>> {
        self.exec.tasks[t].result.clone()
    }

    pub(super) fn fut_poll(&mut self, f: u64) -> Result<u64, i32> {
        let kind = self
            .exec
            .futs
            .get_mut(&f)
            .unwrap_or_else(|| panic!("interp: poll of a freed or unknown future {f:#x}"));
        let ready = match kind {
            Fut::Boxed { poll, done, .. } => {
                assert!(!*done, "interp: poll after READY");
                let poll = *poll;
                let r = self.call_fn(poll, vec![super::V::S(f + 16), super::V::S(CX)])?;
                let r = r.s() as u32;
                if r == 1 {
                    if let Some(Fut::Boxed { done, .. }) = self.exec.futs.get_mut(&f) {
                        *done = true;
                    }
                }
                return Ok(r as u64);
            }
            Fut::Sleep { deadline } => self.exec.now >= *deadline,
            Fut::Yield { polled } => {
                let first = !*polled;
                *polled = true;
                self.exec.woke |= first;
                !first
            }
            Fut::Ready => true,
            Fut::Join { task, size } => {
                let (task, size) = (*task, *size);
                match self.exec.tasks[task].result.take() {
                    Some(bytes) => {
                        self.write_bytes(f + 16, &bytes[..size as usize]);
                        true
                    }
                    None => false,
                }
            }
            Fut::All { .. } => return self.poll_all(f),
            Fut::Started { .. } => return Ok(self.poll_started(f)),
            &mut Fut::Race { size, .. } => return self.poll_race(f, size),
            Fut::Io(_) => return Ok(self.poll_io(f) as u64),
        };
        Ok(ready as u64)
    }

    fn poll_all(&mut self, f: u64) -> Result<u64, i32> {
        let Some(Fut::All {
            children,
            results,
            size,
            reject_early,
            ..
        }) = self.exec.futs.get(&f)
        else {
            unreachable!()
        };
        let (children, results, size, reject_early) =
            (children.clone(), *results, *size, *reject_early);
        let mut left = children.clone();
        let mut rejected = None;
        for (i, c) in children.into_iter().enumerate() {
            if c != 0 && self.fut_poll(c)? == 1 {
                let b = self.read_bytes(c + 16, size as usize);
                self.write_bytes(results + i as u64 * size, &b);
                self.fut_drop(c)?;
                left[i] = 0;
                if reject_early && b.first().is_some_and(|&tag| tag != 0) {
                    if i != 0 {
                        let result_drop = match self.exec.futs.get(&f) {
                            Some(Fut::All { result_drop, .. }) => *result_drop,
                            _ => 0,
                        };
                        if left[0] == 0 && result_drop != 0 {
                            self.call_addr(result_drop, vec![results])?;
                        }
                        self.write_bytes(results, &b);
                    }
                    rejected = Some(i);
                    break;
                }
            }
        }
        let done = rejected.is_some() || left.iter().all(|&c| c == 0);
        if let Some(Fut::All {
            children,
            complete,
            rejected: r,
            ..
        }) = self.exec.futs.get_mut(&f)
        {
            *children = left;
            *complete = done && rejected.is_none();
            *r = rejected;
        }
        Ok(done as u64)
    }

    pub(super) fn fut_drop(&mut self, f: u64) -> Result<(), i32> {
        if let Some(Fut::Started { .. }) = self.exec.futs.get(&f) {
            if self.drop_started(f)? {
                self.heap_free(f);
            }
            return Ok(());
        }
        let kind = self
            .exec
            .futs
            .remove(&f)
            .unwrap_or_else(|| panic!("interp: drop of a freed or unknown future {f:#x}"));
        match kind {
            Fut::Boxed {
                drop, done: false, ..
            } => {
                self.call_fn(drop, vec![super::V::S(f + 16)])?;
            }
            Fut::Race { children, .. } => {
                for c in children {
                    self.fut_drop(c)?;
                }
            }
            Fut::All {
                children,
                results,
                size,
                result_drop,
                complete,
                rejected,
                ..
            } => {
                for (i, c) in children.into_iter().enumerate() {
                    if c != 0 {
                        self.fut_drop(c)?;
                    } else if result_drop != 0
                        && !complete
                        && !rejected.is_some_and(|r| i == 0 || i == r)
                    {
                        self.call_addr(result_drop, vec![results + i as u64 * size])?;
                    }
                }
            }
            _ => {}
        }
        self.heap_free(f);
        Ok(())
    }

    /// Run the root future to completion, driving tasks, started promises and the clock; then
    /// finish the started promises nobody awaited (like a JS program's pending work).
    fn block_on(&mut self, poll: u64, state: u64) -> Result<(), i32> {
        let poll = self.func_id(poll);
        let mut root_done = false;
        self.exec.running = true;
        for _ in 0..MAX_ROUNDS {
            self.exec.woke = false;
            if !root_done {
                let r = self.call_fn(poll, vec![super::V::S(state), super::V::S(CX)])?;
                root_done = r.s() as u32 == 1;
            }
            if root_done && self.exec.locals.is_empty() {
                return Ok(());
            }
            self.run_locals()?;
            self.run_tasks()?;
            if !self.exec.woke {
                self.advance_clock();
            }
        }
        panic!("interp: async program did not finish (deadlock?)")
    }

    /// Poll every unfinished task once; finished tasks keep only their result bytes.
    fn run_tasks(&mut self) -> Result<(), i32> {
        for t in 0..self.exec.tasks.len() {
            let task = &self.exec.tasks[t];
            if task.result.is_some() || task.fut == 0 {
                continue;
            }
            let (fut, size) = (task.fut, task.size);
            if self.fut_poll(fut)? == 1 {
                let bytes = self.read_bytes(fut + 16, size as usize);
                self.fut_drop(fut)?;
                self.exec.tasks[t].result = Some(bytes);
                self.exec.tasks[t].fut = 0;
                self.exec.woke = true;
            }
        }
        Ok(())
    }

    fn advance_clock(&mut self) {
        let next = self
            .exec
            .futs
            .values()
            .filter_map(|f| match f {
                Fut::Sleep { deadline } if *deadline > self.exec.now => Some(*deadline),
                _ => None,
            })
            .fold(f64::INFINITY, f64::min);
        assert!(
            next.is_finite(),
            "interp: all futures pending with nothing to wait for"
        );
        self.exec.now = next;
    }
}

//! Emulated eager promises (`velt_rt_fut_start`) and `velt_rt_race` for the interpreter
//! (rt_abi_async.md §1). A started promise is polled once right away and then on every round of
//! `block_on` until it finishes, whether or not someone awaits it; its owner only observes
//! completion. Dropped unfinished, it keeps running and its result is dropped when it finishes
//! (JS semantics); `block_on` keeps going until every started promise finished.

use super::async_rt::{Fut, CX};
use super::{Interp, V};

impl Interp<'_> {
    /// `velt_rt_fut_start(f, result_drop)`: start a boxed state now (no-op for anything else, and
    /// outside `block_on`).
    pub(super) fn fut_start(&mut self, f: u64, result_drop: u64) -> Result<u64, i32> {
        let Some(&Fut::Boxed {
            poll, done: false, ..
        }) = self.exec.futs.get(&f)
        else {
            return Ok(0);
        };
        if !self.exec.running {
            return Ok(0);
        }
        let kind = Fut::Started {
            poll,
            result_drop,
            done: false,
            delivered: false,
            detached: false,
        };
        self.exec.futs.insert(f, kind);
        self.exec.locals.push(f);
        self.run_started(f)?;
        Ok(0)
    }

    /// Poll started promise `f` once (if still running); on completion, dispose of the result
    /// of a detached one.
    fn run_started(&mut self, f: u64) -> Result<(), i32> {
        let Some(&Fut::Started {
            poll, done: false, ..
        }) = self.exec.futs.get(&f)
        else {
            return Ok(());
        };
        let r = self.call_fn(poll, vec![V::S(f + 16), V::S(CX)])?.s() as u32;
        if r != 1 {
            return Ok(());
        }
        self.exec.locals.retain(|&l| l != f);
        // Its owner may be waiting: another round before the clock moves.
        self.exec.woke = true;
        let Some(Fut::Started {
            done,
            detached,
            result_drop,
            ..
        }) = self.exec.futs.get_mut(&f)
        else {
            unreachable!()
        };
        *done = true;
        if *detached {
            let rd = *result_drop;
            self.exec.futs.remove(&f);
            if rd != 0 {
                self.call_addr(rd, vec![f + 16])?;
            }
            self.heap_free(f);
        }
        Ok(())
    }

    /// The owner polls started promise `f`: ready once it finished.
    pub(super) fn poll_started(&mut self, f: u64) -> u64 {
        match self.exec.futs.get_mut(&f) {
            Some(Fut::Started {
                done: true,
                delivered,
                ..
            }) => {
                *delivered = true;
                1
            }
            _ => 0,
        }
    }

    /// The owner drops started promise `f`: true when it can be freed now (finished), else it
    /// keeps running detached.
    pub(super) fn drop_started(&mut self, f: u64) -> Result<bool, i32> {
        let Some(Fut::Started {
            done,
            delivered,
            detached,
            result_drop,
            ..
        }) = self.exec.futs.get_mut(&f)
        else {
            unreachable!()
        };
        if !*done {
            *detached = true;
            return Ok(false);
        }
        let (rd, delivered) = (*result_drop, *delivered);
        self.exec.futs.remove(&f);
        if !delivered && rd != 0 {
            self.call_addr(rd, vec![f + 16])?;
        }
        Ok(true)
    }

    /// Poll every started, unfinished promise once (a `block_on` round).
    pub(super) fn run_locals(&mut self) -> Result<(), i32> {
        for f in self.exec.locals.clone() {
            self.run_started(f)?;
        }
        Ok(())
    }

    /// `velt_rt_race(futs, n, size)` / `velt_rt_race_ok(.., reject_drop)` (`first_ok`).
    pub(super) fn rt_race(&mut self, a: &[u64], first_ok: Option<u64>) -> u64 {
        let children = (0..a[1]).map(|i| self.read_u64(a[0] + 8 * i)).collect();
        let kind = Fut::Race {
            children,
            size: a[2],
            first_ok,
        };
        self.new_fut(a[2], kind)
    }

    /// Poll a race: the first finished child's result moves into the race's slot (with
    /// `first_ok`, a rejected one is dropped instead while other children are running).
    pub(super) fn poll_race(&mut self, f: u64, size: u64) -> Result<u64, i32> {
        let Some(Fut::Race {
            children, first_ok, ..
        }) = self.exec.futs.get(&f)
        else {
            unreachable!()
        };
        let (mut children, first_ok) = (children.clone(), *first_ok);
        let mut i = 0;
        while i < children.len() {
            let c = children[i];
            if self.fut_poll(c)? != 1 {
                i += 1;
                continue;
            }
            children.remove(i);
            let rejected = self.read_bytes(c + 16, 1)[0] != 0;
            if let (Some(d), true) = (first_ok, rejected && !children.is_empty()) {
                if d != 0 {
                    self.call_addr(d, vec![c + 16])?;
                }
                self.fut_drop(c)?;
                continue;
            }
            let b = self.read_bytes(c + 16, size as usize);
            self.write_bytes(f + 16, &b);
            self.fut_drop(c)?;
            for &d in &children {
                self.fut_drop(d)?;
            }
            self.exec.futs.insert(f, Fut::Ready);
            return Ok(1);
        }
        if let Some(Fut::Race { children: left, .. }) = self.exec.futs.get_mut(&f) {
            *left = children;
        }
        Ok(0)
    }
}

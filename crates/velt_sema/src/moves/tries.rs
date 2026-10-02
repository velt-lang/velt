//! `try` / `catch` / `finally` in the moves dataflow, and the statements that leave through a
//! `finally`.
//!
//! A `finally` runs on every way out of its `try` and `catch`: at their end, after a throw, and
//! before each `return`, `break` or `continue` that leaves them. While a `try` is open, such
//! exits are recorded in its [`TryFrame`] with their state instead of going to their target; the
//! `finally` is then checked once (with reporting) from the join of every way into it, so a value
//! it uses is shared, not moved, wherever it was passed on before. The state after that run goes
//! on to each recorded target (the function's exit, the loop of a `break` / `continue`, through
//! enclosing `finally`s first) and, for a throw, to the enclosing handler and `finally`. The code
//! after the `try` continues from the `finally` run on the normal path alone (silently).
//!
//! A throw can leave a `try` body anywhere: its handler starts from the join of the states at
//! entry, at the end of the body, and at the throws recorded inside it (explicit `throw`s and
//! throws coming out of a nested `try` through its `finally`).

use super::state::{join, Flow};
use super::Moves;
use crate::hir::{Block, LocalId};

/// Where a recorded exit was going.
#[derive(Clone, Copy)]
pub(super) enum Exit {
    Return,
    /// `break` / `continue` to `Moves::loops[index]`.
    Jump {
        index: usize,
        is_break: bool,
    },
}

/// One open `try` body or `catch` handler.
pub(super) struct TryFrame {
    /// Does a `finally` run when control leaves this region?
    finally: bool,
    /// `Moves::loops.len()` when the region started: loops at lower indices are outside it.
    loops: usize,
    /// Exits through this region's `finally`, with their state.
    exits: Vec<(Exit, Flow)>,
    /// States at which a throw leaves a nested region into this one.
    thrown: Flow,
}

impl Moves<'_> {
    pub(super) fn try_stmt(
        &mut self,
        body: &Block,
        catch: Option<&(Option<LocalId>, Block)>,
        finally: Option<&Block>,
        st: &mut Flow,
    ) {
        let has_finally = finally.is_some();
        let entry = st.clone();
        let body_frame = self.region(body, has_finally, st);
        // Every state a throw out of the body can have.
        let body_throws = join(join(entry, st.clone()), body_frame.thrown);
        let mut exits = body_frame.exits;
        let mut escaping = None;
        match catch {
            Some((local, handler)) => {
                let mut h = body_throws;
                if let Some(l) = local {
                    Self::init_local(*l, &mut h);
                }
                let h_entry = h.clone();
                let handler_frame = self.region(handler, has_finally, &mut h);
                exits.extend(handler_frame.exits);
                escaping = join(join(h_entry, h.clone()), handler_frame.thrown);
                *st = join(st.take(), h);
            }
            None => escaping = body_throws,
        }
        let Some(f) = finally else {
            // Throws leave this `try` as they are: the enclosing region sees them at its own
            // throw points and its entry / end.
            return;
        };
        let mut every = escaping.clone();
        for (_, s) in &exits {
            every = join(every, s.clone());
        }
        every = join(every, st.clone());
        self.block(f, &mut every);
        // After the `finally`: on to each exit's target, and a throw on to the enclosing region.
        for (exit, _) in exits {
            let mut s = every.clone();
            match exit {
                Exit::Return => self.leave_return(&mut s),
                Exit::Jump { index, is_break } => self.leave_jump(index, is_break, &mut s),
            }
        }
        if escaping.is_some() {
            self.record_throw(&every);
        }
        let report = std::mem::replace(&mut self.report, false);
        self.block(f, st);
        self.report = report;
    }

    /// Check `b` as a region of a `try` (its body or handler); returns what left it.
    fn region(&mut self, b: &Block, finally: bool, st: &mut Flow) -> TryFrame {
        self.tries.push(TryFrame {
            finally,
            loops: self.loops.len(),
            exits: vec![],
            thrown: None,
        });
        self.block(b, st);
        self.tries.pop().expect("ICE: try frame stack")
    }

    /// A throw (an explicit one, or one out of a nested `finally`) at state `st`.
    pub(super) fn record_throw(&mut self, st: &Flow) {
        if let Some(top) = self.tries.last_mut() {
            top.thrown = join(top.thrown.take(), st.clone());
        }
    }

    /// `return` at state `st`: through the innermost `finally`, if any.
    pub(super) fn leave_return(&mut self, st: &mut Flow) {
        if let Some(frame) = self.tries.iter_mut().rev().find(|f| f.finally) {
            frame.exits.push((Exit::Return, st.clone()));
        }
        *st = None;
    }

    /// `break` / `continue` to loop `index` at state `st`: through the innermost `finally`
    /// between here and that loop, if any, else straight to the loop.
    pub(super) fn leave_jump(&mut self, index: usize, is_break: bool, st: &mut Flow) {
        let through = self
            .tries
            .iter_mut()
            .rev()
            .take_while(|f| f.loops > index)
            .find(|f| f.finally);
        match through {
            Some(frame) => {
                frame
                    .exits
                    .push((Exit::Jump { index, is_break }, st.clone()));
                *st = None;
            }
            None => self.jump_to(index, is_break, st),
        }
    }
}

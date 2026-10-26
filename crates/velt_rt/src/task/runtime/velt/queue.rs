//! A worker's run queue: input first (tasks woken by data to read, timers or other threads),
//! then output and the work tasks made (writers woken by room to write; spawned, woken by a task
//! of this worker, or woken while they ran), each in the order it came.
//!
//! Input before output: a task woken by its socket (a message to publish) runs before the
//! flushers it starts for the subscribers, so output queued meanwhile leaves in fewer writes
//! (one worker broadcasting to 1000 connections: 7.0M deliveries/s, 1.1M when the flushers ran
//! as often as input). Each queue keeps its order: woken tasks put at the front, as an earlier
//! version did, overtook the tasks woken before them, and under constant I/O those waited
//! without bound (JSON on 512 connections: p99 1.8 s).
//!
//! A task woken by the task running (a query handed to a connection's driver, its reply to the
//! request waiting for it) runs next, from a one-task slot ([`push_next`](RunQueue::push_next)),
//! as with tokio's LIFO slot: a chain of request and reply between tasks does not wait behind
//! the whole queue at each step (a connection pool's queries are its bottleneck: `/db` on 18
//! connections ran at half of tokio's rate without it). At most [`NEXT_RUNS`] in a row, so two
//! tasks waking each other cannot hold the worker.
//!
//! The second queue waits only while input never runs out, and not for long: every
//! [`TRICKLE`]th task comes from it (a spawned task that a request awaits runs within a few
//! dozen polls), and once it has waited [`FAIR`], a fair pass runs every task it holds.

use super::task::Task;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long the second queue may wait behind the first before a fair pass. Shorter costs the
/// broadcast its batching (2 ms: 4.5M deliveries/s of the 7.0M above).
const FAIR: Duration = Duration::from_millis(10);
/// One task in this many comes from the second queue while both hold tasks (tokio takes from
/// its global queue as often).
const TRICKLE: u32 = 61;
/// Tasks in a row from the slot for the next task, before the queue gets a turn (tokio's
/// limit too).
const NEXT_RUNS: u32 = 3;

pub(super) struct RunQueue {
    /// Input: woken by I/O, timers or other threads.
    io: VecDeque<Arc<Task>>,
    /// Made by this worker's tasks: spawned, woken by a task, or woken while it ran.
    rest: VecDeque<Arc<Task>>,
    /// Woken by the task running: runs next.
    next: Option<Arc<Task>>,
    /// Tasks run from `next` in a row.
    next_runs: u32,
    /// Tasks taken from `io` since the last from `rest`.
    since_rest: u32,
    /// Tasks of `rest` left in the current fair pass (0: none running).
    fair_left: usize,
    /// When the next fair pass is due, counted from when `rest` last ran empty or a pass ended.
    fair_due: Instant,
}

impl RunQueue {
    pub(super) fn new(now: Instant) -> RunQueue {
        RunQueue {
            io: VecDeque::new(),
            rest: VecDeque::new(),
            next: None,
            next_runs: 0,
            since_rest: 0,
            fair_left: 0,
            fair_due: now + FAIR,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.io.len() + self.rest.len() + usize::from(self.next.is_some())
    }

    /// Input waiting (the first queue and the next task).
    pub(super) fn input(&self) -> usize {
        self.io.len() + usize::from(self.next.is_some())
    }

    /// Makes `task` (woken by the task running) the next to run; one there before it joins
    /// the input.
    pub(super) fn push_next(&mut self, task: Arc<Task>) {
        if let Some(before) = self.next.replace(task) {
            self.io.push_back(before);
        }
    }

    /// Queues input (in the order it came).
    pub(super) fn extend_io(&mut self, tasks: impl IntoIterator<Item = Arc<Task>>) {
        self.io.extend(tasks);
    }

    /// Queues work a task of this worker made.
    pub(super) fn push(&mut self, task: Arc<Task>, now: impl FnOnce() -> Instant) {
        if self.rest.is_empty() {
            self.fair_due = now() + FAIR;
        }
        self.rest.push_back(task);
    }

    /// Queues work that is not input: output (writers woken by room to write) and tasks moved
    /// over from another worker.
    pub(super) fn extend(&mut self, tasks: impl IntoIterator<Item = Arc<Task>>, now: Instant) {
        let was_empty = self.rest.is_empty();
        self.rest.extend(tasks);
        if was_empty && !self.rest.is_empty() {
            self.fair_due = now + FAIR;
        }
    }

    /// Starts a fair pass if one is due (called at each reactor turn, with its time).
    pub(super) fn tick(&mut self, now: Instant) {
        if self.fair_left == 0
            && !self.rest.is_empty()
            && !self.io.is_empty()
            && now >= self.fair_due
        {
            self.fair_left = self.rest.len();
        }
    }

    /// The next task to run.
    pub(super) fn pop(&mut self, now: impl FnOnce() -> Instant) -> Option<Arc<Task>> {
        if self.fair_left > 0 {
            self.fair_left -= 1;
            if self.fair_left == 0 {
                self.fair_due = now() + FAIR;
            }
            if let Some(t) = self.rest.pop_front() {
                return Some(t);
            }
            self.fair_left = 0;
        }
        if let Some(t) = self.next.take() {
            if self.next_runs < NEXT_RUNS {
                self.next_runs += 1;
                return Some(t);
            }
            self.io.push_back(t);
        }
        self.next_runs = 0;
        if self.since_rest >= TRICKLE || self.io.is_empty() {
            if let Some(t) = self.rest.pop_front() {
                self.since_rest = 0;
                return Some(t);
            }
        }
        let t = self.io.pop_front()?;
        self.since_rest += 1;
        Some(t)
    }

    /// Whether a spawn burst left tasks waiting behind the one about to run: the length of the
    /// second queue when its first or last task is fresh (spawned and never polled; the first
    /// is the sibling of a spawned task just taken, the last a spawn of the task that ran
    /// before), else 0.
    pub(super) fn fresh_behind(&self) -> usize {
        let fresh = |t: Option<&Arc<Task>>| t.is_some_and(|t| t.fresh());
        if fresh(self.rest.front()) || fresh(self.rest.back()) {
            self.rest.len()
        } else {
            0
        }
    }

    /// Takes up to `max` fresh tasks that are not pinned from the first `within` of the second
    /// queue, oldest first (for idle workers to start: see `worker`'s watch).
    pub(super) fn take_fresh(&mut self, within: usize, max: usize) -> Vec<Arc<Task>> {
        let mut taken = Vec::new();
        let (mut i, mut end) = (0, within.min(self.rest.len()));
        while i < end && taken.len() < max {
            if self.rest[i].fresh() && !self.rest[i].pinned {
                taken.extend(self.rest.remove(i));
                end -= 1;
            } else {
                i += 1;
            }
        }
        self.fair_left = self.fair_left.saturating_sub(taken.len());
        taken
    }

    /// Takes the half of the input that has waited longest (for a worker to borrow; the next
    /// task stays).
    pub(super) fn take_half_input(&mut self) -> Vec<Arc<Task>> {
        let n = self.io.len() / 2;
        self.io.drain(..n).collect()
    }

    /// Takes the half that has waited longest from each queue (to offer to a helper), rounded
    /// down: the worker keeps one of a single task.
    pub(super) fn take_half(&mut self) -> Vec<Arc<Task>> {
        self.take(self.io.len() / 2, self.rest.len() / 2)
    }

    /// Takes the half that has waited longest from each queue, rounded up (from a stalled
    /// worker: a single task waiting must move too).
    pub(super) fn take_half_up(&mut self) -> Vec<Arc<Task>> {
        if let Some(t) = self.next.take() {
            self.io.push_front(t);
        }
        self.take(self.io.len().div_ceil(2), self.rest.len().div_ceil(2))
    }

    fn take(&mut self, io: usize, rest: usize) -> Vec<Arc<Task>> {
        self.fair_left = self.fair_left.saturating_sub(rest);
        let mut half: Vec<_> = self.io.drain(..io).collect();
        half.extend(self.rest.drain(..rest));
        half
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn task(id: usize) -> Arc<Task> {
        Task::new(Box::pin(async {}), id, false)
    }

    fn id(t: Option<Arc<Task>>) -> usize {
        t.map_or(usize::MAX, |t| t.home.load(Ordering::Relaxed))
    }

    #[test]
    fn io_wakes_run_first_each_queue_in_order() {
        let t = Instant::now();
        let mut q = RunQueue::new(t);
        q.push(task(10), || t);
        q.extend_io([task(1), task(2)]);
        q.extend_io([task(3)]);
        q.push(task(11), || t);
        let order: Vec<_> = (0..5).map(|_| id(q.pop(|| t))).collect();
        assert_eq!(order, [1, 2, 3, 10, 11]);
        assert!(q.pop(|| t).is_none());
    }

    #[test]
    fn work_made_by_tasks_trickles_in_between_input() {
        let t = Instant::now();
        let mut q = RunQueue::new(t);
        q.extend_io((0..200).map(task));
        q.push(task(1000), || t);
        let at = (0..201).position(|_| id(q.pop(|| t)) == 1000);
        assert_eq!(at, Some(TRICKLE as usize));
    }

    #[test]
    fn a_fair_pass_runs_what_waited_behind_constant_io() {
        let t = Instant::now();
        let mut q = RunQueue::new(t);
        q.push(task(10), || t);
        q.push(task(11), || t);
        q.extend_io([task(1)]);
        // Not due yet: I/O first.
        q.tick(t + FAIR / 2);
        assert_eq!(id(q.pop(|| t)), 1);
        // Due: both waiting tasks run before the next I/O wake.
        q.extend_io([task(2)]);
        q.tick(t + FAIR);
        q.push(task(12), || t);
        assert_eq!(id(q.pop(|| t)), 10);
        assert_eq!(id(q.pop(|| t)), 11);
        assert_eq!(id(q.pop(|| t + FAIR)), 2);
        assert_eq!(id(q.pop(|| t)), 12);
    }

    #[test]
    fn a_task_woken_by_the_running_one_runs_next_three_in_a_row_at_most() {
        let t = Instant::now();
        let mut q = RunQueue::new(t);
        q.extend_io([task(1)]);
        q.push_next(task(10));
        // A newer wake takes the slot; the one before joins the input.
        q.push_next(task(11));
        assert_eq!(id(q.pop(|| t)), 11);
        q.push_next(task(12));
        assert_eq!(id(q.pop(|| t)), 12);
        q.push_next(task(13));
        assert_eq!(id(q.pop(|| t)), 13);
        // A fourth in a row waits its turn behind the input.
        q.push_next(task(14));
        let order: Vec<_> = (0..3).map(|_| id(q.pop(|| t))).collect();
        assert_eq!(order, [1, 10, 14]);
    }

    #[test]
    fn tasks_never_polled_are_taken_from_those_queued_before() {
        let t = Instant::now();
        let mut q = RunQueue::new(t);
        let polled = task(2);
        assert!(matches!(polled.run(), super::super::task::Ran::Done));
        let pinned = Task::new(Box::pin(async {}), 3, true);
        q.extend_io([task(100)]);
        for t2 in [task(1), polled, pinned, task(4), task(5)] {
            q.push(t2, || t);
        }
        // Only the first four were queued before: of those, the fresh ones not pinned.
        let taken: Vec<_> = q
            .take_fresh(4, 9)
            .into_iter()
            .map(|t| id(Some(t)))
            .collect();
        assert_eq!(taken, [1, 4]);
        let mut q2 = RunQueue::new(t);
        for i in 1..=3 {
            q2.push(task(i), || t);
        }
        assert_eq!(q2.fresh_behind(), 3);
        // At most as many as asked for, oldest first.
        let taken: Vec<_> = q2
            .take_fresh(3, 2)
            .into_iter()
            .map(|t| id(Some(t)))
            .collect();
        assert_eq!(taken, [1, 2]);
        let order: Vec<_> = (0..4).map(|_| id(q.pop(|| t))).collect();
        assert_eq!(order, [100, 2, 3, 5]);
        // Nothing fresh at either end: no burst behind.
        assert_eq!(q.fresh_behind(), 0);
    }

    #[test]
    fn half_of_each_queue_is_taken_oldest_first() {
        let t = Instant::now();
        let mut q = RunQueue::new(t);
        q.extend_io((1..=4).map(task));
        q.extend((10..=11).map(task), t);
        let half: Vec<_> = q.take_half().into_iter().map(|t| id(Some(t))).collect();
        assert_eq!(half, [1, 2, 10]);
        assert_eq!(q.len(), 3);
        // From a stalled worker, the last of each queue moves too.
        let up: Vec<_> = q.take_half_up().into_iter().map(|t| id(Some(t))).collect();
        assert_eq!(up, [3, 11]);
    }
}

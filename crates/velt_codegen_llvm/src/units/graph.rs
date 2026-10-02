//! The reference graph of a program: what each function and static refers to (`Refs`), the
//! graph over functions and statics built from it, and its strongly connected components.

use std::collections::BTreeSet;

use velt_vir::vir::{self, Callee, Const, Operand, Rvalue, Stmt, Terminator};

/// Program entities a function or static refers to.
#[derive(Default)]
pub(super) struct Refs {
    /// Functions called directly.
    pub calls: BTreeSet<usize>,
    /// Functions whose address is taken.
    pub addresses: BTreeSet<usize>,
    pub statics: BTreeSet<usize>,
}

impl Refs {
    /// Without references to functions ≥ `funcs` or statics ≥ `statics`.
    pub fn within(mut self, funcs: usize, statics: usize) -> Refs {
        self.calls.retain(|&f| f < funcs);
        self.addresses.retain(|&f| f < funcs);
        self.statics.retain(|&s| s < statics);
        self
    }

    /// Functions called or whose address is taken.
    pub fn funcs(&self) -> impl Iterator<Item = usize> + '_ {
        self.calls.iter().chain(&self.addresses).copied()
    }

    pub fn of_function(f: &vir::Function) -> Refs {
        let mut r = Refs::default();
        for b in &f.blocks {
            b.stmts.iter().for_each(|s| r.stmt(s));
            match &b.term {
                Terminator::Branch { cond: o, .. }
                | Terminator::Switch { value: o, .. }
                | Terminator::Return(o) => r.operand(o),
                Terminator::Call { callee, args, .. } => {
                    match callee {
                        Callee::Func(id) => {
                            r.calls.insert(id.0 as usize);
                        }
                        Callee::Extern(_) => {}
                        Callee::Ptr { target, .. } => r.operand(target),
                    }
                    args.iter().for_each(|o| r.operand(o));
                }
                Terminator::Goto(_) | Terminator::Unreachable => {}
            }
        }
        r
    }

    pub fn of_static(s: &vir::StaticData) -> Refs {
        let mut r = Refs::default();
        for (_, target) in &s.relocs {
            r.constant(target);
        }
        r
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Assign(_, rv) => match rv {
                Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => self.operand(a),
                Rvalue::Binary(_, a, b) => {
                    self.operand(a);
                    self.operand(b);
                }
                Rvalue::Aggregate(_, ops) => ops.iter().for_each(|o| self.operand(o)),
                Rvalue::AddrOf(_) => {}
            },
            Stmt::MemCopy { dst, src, .. } => {
                self.operand(dst);
                self.operand(src);
            }
            Stmt::MemCopyDyn { dst, src, len, .. } => {
                self.operand(dst);
                self.operand(src);
                self.operand(len);
            }
            Stmt::MemSet { dst, byte, len } => {
                self.operand(dst);
                self.operand(byte);
                self.operand(len);
            }
            Stmt::Nop => {}
        }
    }

    fn operand(&mut self, o: &Operand) {
        if let Operand::Const(c, _) = o {
            self.constant(c);
        }
    }

    fn constant(&mut self, c: &Const) {
        match c {
            Const::Func(id) => {
                self.addresses.insert(id.0 as usize);
            }
            Const::Static(id) => {
                self.statics.insert(id.0 as usize);
            }
            _ => {}
        }
    }
}

/// Successors of every node: functions are nodes `0..funcs.len()`, statics follow them. A
/// function's edges are its calls, taken addresses and statics; a static's are its relocations.
pub(super) fn successors(funcs: &[Refs], statics: &[Refs]) -> Vec<Vec<usize>> {
    let n = funcs.len();
    let edges = |r: &Refs| -> Vec<usize> {
        let mut out: Vec<usize> = r.funcs().chain(r.statics.iter().map(|&s| n + s)).collect();
        out.sort_unstable();
        out.dedup();
        out
    };
    funcs.iter().chain(statics).map(edges).collect()
}

/// Strongly connected components (Tarjan's algorithm, iterative, so deep call chains cannot
/// overflow the stack): the component of every node, numbered so that a component comes after
/// every component it reaches (callees before callers), and the number of components.
pub(super) fn components(succ: &[Vec<usize>]) -> (Vec<usize>, usize) {
    let mut t = Tarjan {
        index: vec![UNVISITED; succ.len()],
        low: vec![0; succ.len()],
        on_stack: vec![false; succ.len()],
        comp: vec![0; succ.len()],
        stack: Vec::new(),
        frames: Vec::new(),
        next: 0,
        count: 0,
    };
    for root in 0..succ.len() {
        if t.index[root] == UNVISITED {
            t.visit(root);
            t.run(succ);
        }
    }
    (t.comp, t.count)
}

const UNVISITED: usize = usize::MAX;

/// State of [`components`].
struct Tarjan {
    /// Visit order of each node (`UNVISITED` before).
    index: Vec<usize>,
    /// Lowest visit order reachable from the node's subtree through nodes on the stack.
    low: Vec<usize>,
    on_stack: Vec<bool>,
    comp: Vec<usize>,
    stack: Vec<usize>,
    /// The depth-first path: node and its next successor to look at.
    frames: Vec<(usize, usize)>,
    next: usize,
    count: usize,
}

impl Tarjan {
    fn visit(&mut self, v: usize) {
        self.index[v] = self.next;
        self.low[v] = self.next;
        self.next += 1;
        self.stack.push(v);
        self.on_stack[v] = true;
        self.frames.push((v, 0));
    }

    /// Depth-first search until the path is empty.
    fn run(&mut self, succ: &[Vec<usize>]) {
        while let Some((v, i)) = self.frames.last_mut().map(|f| {
            f.1 += 1;
            (f.0, f.1 - 1)
        }) {
            if let Some(&w) = succ[v].get(i) {
                if self.index[w] == UNVISITED {
                    self.visit(w);
                } else if self.on_stack[w] {
                    self.low[v] = self.low[v].min(self.index[w]);
                }
                continue;
            }
            self.frames.pop();
            if let Some(&(u, _)) = self.frames.last() {
                self.low[u] = self.low[u].min(self.low[v]);
            }
            if self.low[v] == self.index[v] {
                while let Some(w) = self.stack.pop() {
                    self.on_stack[w] = false;
                    self.comp[w] = self.count;
                    if w == v {
                        break;
                    }
                }
                self.count += 1;
            }
        }
    }
}

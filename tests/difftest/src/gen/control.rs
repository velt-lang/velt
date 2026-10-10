//! Blocks and control flow: `if`/`else`, counted `for`, `while`/`do…while` (the counter is bumped
//! first, so `continue` can't loop forever), `for...of` over arrays and maps, `try`/`catch`/
//! `finally` and guarded `break`/`continue` (possibly to an enclosing labeled loop).

use super::scope::Ty;
use super::Gen;

/// Deepest block nesting generated.
const MAX_DEPTH: usize = 4;

impl Gen {
    /// Up to `max` statements, as long as the function's budget lasts.
    pub(super) fn block(&mut self, max: usize) {
        let n = self.rng.range(1, max.max(1) as i64);
        for _ in 0..n {
            if self.budget == 0 {
                return;
            }
            self.budget -= 1;
            if self.depth < MAX_DEPTH && self.rng.chance(25) {
                self.control_stmt();
            } else {
                self.simple_stmt();
            }
        }
    }

    fn control_stmt(&mut self) {
        match self.rng.below(11) {
            0..=1 => self.if_stmt(),
            2 => self.for_stmt(),
            3 => self.while_stmt(),
            4..=5 => self.for_of_stmt(),
            6 => self.try_stmt(),
            7..=8 => self.switch_stmt(),
            _ if self.loops > 0 => {
                let kw = *self.rng.pick(&["break", "continue"]);
                let labels: Vec<String> = self.labels.iter().flatten().cloned().collect();
                let target = match labels.is_empty() || self.rng.chance(50) {
                    true => String::new(),
                    false => format!(" {}", self.rng.pick(&labels)),
                };
                let c = self.boolean(2);
                self.line(&format!("if ({c}) {{ {kw}{target}; }}"));
            }
            _ if self.in_try => self.throw_stmt(),
            _ if self.rng.chance(40) => self.early_return_stmt(),
            _ => self.print_stmt(),
        }
    }

    fn if_stmt(&mut self) {
        let c = self.boolean(3);
        self.open(&format!("if ({c}) {{"));
        self.block(4);
        if self.rng.chance(30) {
            self.close("}");
            self.indent_back();
            let c = self.boolean(3);
            self.open(&format!("}} else if ({c}) {{"));
            self.block(3);
        }
        if self.rng.chance(40) {
            self.close("}");
            self.indent_back();
            self.open("} else {");
            self.block(3);
        }
        self.close("}");
    }

    /// Drops the `}` line just emitted so a continuation (`} else {`) can replace it.
    fn indent_back(&mut self) {
        let trimmed = self.out.trim_end_matches('\n');
        let cut = trimmed.rfind('\n').map_or(0, |i| i + 1);
        self.out.truncate(cut);
    }

    fn for_stmt(&mut self) {
        let i = self.scope.fresh("i");
        let n = self.rng.range(0, 6);
        let label = self.new_label();
        let prefix = label.as_ref().map_or(String::new(), |l| format!("{l}: "));
        if self.rng.chance(30) {
            // Comma-separated declarations and updates.
            let j = self.scope.fresh("j");
            self.open(&format!(
                "{prefix}for (let {i}: i64 = 0, {j}: i64 = {n}; {i} < {j}; {i}++, {j}--) {{"
            ));
            self.scope.declare(&j, Ty::Int, false, false);
        } else {
            self.open(&format!("{prefix}for (let {i}: i64 = 0; {i} < {n}; {i}++) {{"));
        }
        self.scope.declare(&i, Ty::Int, false, false);
        self.labels.push(label);
        self.loop_body();
        self.labels.pop();
        self.close("}");
    }

    /// A fresh label for a loop (sometimes), only useful when a loop may nest inside it.
    fn new_label(&mut self) -> Option<String> {
        self.rng.chance(50).then(|| self.scope.fresh("L"))
    }

    fn while_stmt(&mut self) {
        let w = self.scope.fresh("w");
        let n = self.rng.range(0, 6);
        self.line(&format!("let {w}: i64 = 0;"));
        self.scope.declare(&w, Ty::Int, false, false);
        let label = self.new_label();
        let prefix = label.as_ref().map_or(String::new(), |l| format!("{l}: "));
        self.labels.push(label);
        if self.rng.chance(70) {
            self.open(&format!("{prefix}while ({w} < {n}) {{"));
            self.line(&format!("{w}++;"));
            self.loop_body();
            self.close("}");
        } else {
            self.open(&format!("{prefix}do {{"));
            self.line(&format!("{w}++;"));
            self.loop_body();
            self.close(&format!("}} while ({w} < {n});"));
        }
        self.labels.pop();
    }

    fn loop_body(&mut self) {
        self.loops += 1;
        self.block(4);
        self.loops -= 1;
    }

    fn for_of_stmt(&mut self) {
        if let Some(m) = self.pick_var(Ty::Map).filter(|_| self.rng.chance(30)) {
            let (k, v) = (self.scope.fresh("k"), self.scope.fresh("e"));
            let mark = self.scope.exclude(&m);
            self.open(&format!("for (const [{k}, {v}] of {m}) {{"));
            self.scope.declare(&k, Ty::Str, false, false);
            self.scope.declare(&v, Ty::Int, false, false);
            self.labels.push(None);
            self.loop_body();
            self.labels.pop();
            self.close("}");
            self.scope.restore(mark);
            return;
        }
        let ty = *self
            .rng
            .pick(&[Ty::IntArr, Ty::StrArr, Ty::FloatArr, Ty::ObjArr]);
        let xs = self.array(ty, 2);
        let e = self.scope.fresh("e");
        let root = xs.text.split(['.', '[']).next().unwrap_or("").to_string();
        let mark = self.scope.exclude(&root);
        let elem = ty.elem().expect("ICE: array type");
        if self.rng.chance(25) {
            // `entries()` yields `[usize, T]` in Velt: the index is used as `(i as i64)`.
            let i = self.scope.fresh("i");
            self.open(&format!(
                "for (const [{i}, {e}] of {}.entries()) {{",
                xs.text
            ));
            self.scope
                .declare(&format!("({i} as i64)"), Ty::Int, false, false);
        } else {
            self.open(&format!("for (const {e} of {}) {{", xs.text));
        }
        self.scope.declare(&e, elem, false, false);
        self.labels.push(None);
        self.loop_body();
        self.labels.pop();
        self.close("}");
        self.scope.restore(mark);
    }

    fn try_stmt(&mut self) {
        let saved = std::mem::replace(&mut self.in_try, true);
        self.open("try {");
        self.throw_stmt();
        self.block(3);
        self.in_try = saved;
        let e = self.scope.fresh("err");
        self.close("}");
        self.indent_back();
        self.open(&format!("}} catch ({e}) {{"));
        self.line(&format!("console.log(`caught ${{{e}.message}}`);"));
        // Rethrow only from `main`: in a helper it would make callbacks that call it throwing.
        if self.ret.is_none() && self.rng.chance(15) {
            let c = self.boolean(2);
            self.line(&format!("if ({c}) {{ throw {e}; }}"));
        }
        self.block(2);
        if self.rng.chance(30) {
            self.close("}");
            self.indent_back();
            self.open("} finally {");
            self.print_stmt();
        }
        self.close("}");
    }

    /// `if (c) { return …; }` for the current function (through any enclosing `finally`).
    fn early_return_stmt(&mut self) {
        let value = match self.ret {
            Some(Some(ty)) => format!(" {}", self.owned(ty, 2)),
            _ => String::new(),
        };
        let c = self.boolean(2);
        self.line(&format!("if ({c}) {{ return{value}; }}"));
    }

    /// A statement that may throw: a call of a throwing helper, or a conditional `throw`.
    pub(super) fn throw_stmt(&mut self) {
        let throwing: Vec<_> = self
            .funcs
            .iter()
            .filter(|f| f.throws && self.can_call(f))
            .cloned()
            .collect();
        if !throwing.is_empty() && self.rng.chance(70) {
            let sig = self.rng.pick(&throwing).clone();
            let call = self.call_with(&sig, 2);
            self.line(&format!("console.log(`r=${{{call}}}`);"));
            return;
        }
        let (c, n) = (self.boolean(2), self.int(2));
        self.line(&format!("if ({c}) {{ throw new Error(`boom ${{{n}}}`); }}"));
    }
}

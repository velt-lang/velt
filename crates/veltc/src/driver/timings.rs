//! The text of `velt build --timings`.

use std::time::Duration;

use super::{memory, Session};

impl Session {
    /// Per-stage timing report for `--verbose` (with each stage's breakdown for `--timings`).
    pub fn render_timings(&self) -> String {
        let total: Duration = self.timings.iter().map(|(_, d)| *d).sum();
        let mut s = String::new();
        let mut readings = self.memory.iter().peekable();
        for (name, d) in self
            .timings
            .iter()
            .map(|(n, d)| (*n, *d))
            .chain([("total", total)])
        {
            let reading = readings.next_if(|(n, _)| *n == name).map(|(_, r)| *r);
            s.push_str(&format!(
                "velt: {name:<8} {:>9.3} ms",
                d.as_secs_f64() * 1e3
            ));
            match self.cached {
                Some((found, of, what)) if name == "codegen" => {
                    s.push_str(&format!(
                        "  ({found} of {of} {what} from the build cache)\n"
                    ));
                }
                _ if name == "stamp" && !self.timings.iter().any(|(n, _)| *n == "parse") => {
                    s.push_str("  (unchanged: nothing to compile)\n");
                }
                _ => s.push_str(&memory::line_end(reading)),
            }
            let steps = self.details.iter().filter(|(stage, ..)| *stage == name);
            for (_, step, d) in steps.filter(|_| self.show_details) {
                s.push_str(&format!(
                    "velt:   {step:<14} {:>9.3} ms\n",
                    d.as_secs_f64() * 1e3
                ));
            }
        }
        // Nothing loaded (a stamp said the build is unchanged): nothing to say about it.
        if !self.std_files.is_empty() {
            let std_lines: usize = (self.std_files.iter())
                .map(|f| self.sm.get(*f).src.lines().count())
                .sum();
            s.push_str(&format!("velt: {std_lines} std lines loaded\n"));
            s.push_str(&format!(
                "velt: {} sema rounds\n",
                velt_sema::check_rounds()
            ));
        }
        s
    }
}

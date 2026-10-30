"""Unit tests of report.losses on made-up results: `python3 -m unittest discover bench/showcase`
(run.sh runs them before a run)."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import report  # noqa: E402


def impl(label, lang, runs=1, error=None, **summary):
    d = {"label": label, "lang": lang, "summary": {"runs": runs, **summary}}
    if error:
        d["error"] = error
    return d


def results(cat, *recs):
    return {"categories": {cat: list(recs)}}


class Losses(unittest.TestCase):
    def test_faster_competitor_is_listed(self):
        rec = {"name": "fib", "impls": [impl("Velt", "velt", best_wall_s=2.0), impl("Rust", "rust", best_wall_s=1.0),
                                        impl("Node", "node", best_wall_s=3.0)]}
        out = report.losses(results("cpu", rec))
        self.assertEqual(out, ["cpu: fib: Rust (Velt 2.00× slower)"])

    def test_velt_fastest_lists_nothing(self):
        rec = {"name": "fib", "impls": [impl("Velt", "velt", best_wall_s=1.0), impl("Rust", "rust", best_wall_s=2.0)]}
        self.assertEqual(report.losses(results("cpu", rec)), [])

    def test_partial_competitor_counts(self):
        # axum failed in round 4 after 3 runs at 2x Velt's req/s: it is compared, marked partial.
        rec = {"name": "/plaintext", "kind": "http",
               "impls": [impl("Velt", "velt", 5, best_rps=100.0),
                         impl("Rust axum", "rust", 3, error="round 4: exit 1", best_rps=200.0)]}
        out = report.losses(results("http", rec))
        self.assertEqual(len(out), 1)
        self.assertIn("Rust axum (partial: round 4: exit 1)", out[0])
        self.assertIn("Velt 2.00× slower", out[0])

    def test_load_bound_rows_are_marked(self):
        for velt_bound, rust_bound in ((False, True), (True, False)):
            rec = {"name": "/plaintext", "kind": "http",
                   "impls": [impl("Velt", "velt", best_rps=100.0, load_bound=velt_bound),
                             impl("Rust axum", "rust", best_rps=102.0, load_bound=rust_bound)]}
            out = report.losses(results("http", rec))
            self.assertEqual(len(out), 1)
            self.assertTrue(out[0].endswith("(load-bound)"), out[0])

    def test_unbound_rows_are_not_marked(self):
        rec = {"name": "/plaintext", "kind": "http",
               "impls": [impl("Velt", "velt", best_rps=100.0, load_bound=False),
                         impl("Rust axum", "rust", best_rps=150.0, load_bound=False)]}
        out = report.losses(results("http", rec))
        self.assertNotIn("load-bound", out[0])

    def test_competitor_without_runs_is_not_compared(self):
        rec = {"name": "fib", "impls": [impl("Velt", "velt", best_wall_s=2.0),
                                        impl("Rust", "rust", 0, error="check failed: output differs")]}
        self.assertEqual(report.losses(results("cpu", rec)), [])

    def test_all_velt_failed_is_listed(self):
        rec = {"name": "fib", "impls": [impl("Velt", "velt", 0, error="check failed: output differs"),
                                        impl("Rust", "rust", best_wall_s=1.0)]}
        out = report.losses(results("cpu", rec))
        self.assertEqual(out, ["cpu: fib: Velt: n/a (check failed: output differs)"])

    def test_second_velt_variant_used_when_first_failed(self):
        rec = {"name": "nbody", "impls": [impl("Velt main", "velt", 0, error="exit 1"),
                                          impl("Velt main_opt", "velt", best_wall_s=1.0),
                                          impl("Rust", "rust", best_wall_s=0.5)]}
        out = report.losses(results("benchmarks-game", rec))
        self.assertEqual(out, ["benchmarks-game: nbody: Rust (Velt 2.00× slower)"])

    def test_failed_benchmark_record_is_listed(self):
        rec = {"name": "binary-trees", "error": "KeyError: 'N'", "impls": []}
        out = report.losses(results("benchmarks-game", rec))
        self.assertEqual(out, ["benchmarks-game: binary-trees: Velt: n/a (the benchmark failed: KeyError: 'N')"])

    def test_reported_metrics_each_compared(self):
        rec = {"name": "db", "parse": "results", "unit": "ops/s",
               "impls": [impl("Velt", "velt", reported={"a": 100.0, "b": 100.0}),
                         impl("Rust", "rust", reported={"a": 50.0, "b": 300.0})]}
        out = report.losses(results("sqlite", rec))
        self.assertEqual(out, ["sqlite: db: b: Rust (Velt 3.00× slower)"])

    def test_velt_missing_a_reported_workload_is_listed(self):
        rec = {"name": "db", "parse": "results", "unit": "ops/s",
               "impls": [impl("Velt", "velt", reported={"a": 100.0}),
                         impl("Rust", "rust", reported={"a": 50.0, "b": 300.0})]}
        out = report.losses(results("sqlite", rec))
        self.assertEqual(out, ["sqlite: db: b: Velt: n/a (no result)"])

    def test_websocket_lower_p99_wins(self):
        rec = {"name": "chat", "kind": "websocket",
               "impls": [impl("Velt", "velt", p99_ms=10.0), impl("Bun", "bun", p99_ms=5.0)]}
        out = report.losses(results("websocket", rec))
        self.assertEqual(out, ["websocket: chat: p99: Bun (Velt 2.00× slower)"])

    def test_compile_is_not_ranked(self):
        rec = {"name": "hello", "impls": [impl("velt build", "velt", best_wall_s=2.0), impl("rustc", "rust", best_wall_s=1.0)]}
        self.assertEqual(report.losses(results("compile", rec)), [])


if __name__ == "__main__":
    unittest.main()

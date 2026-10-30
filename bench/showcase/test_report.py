"""Unit tests of report.losses and the like-for-like pairing on made-up results: `python3 -m unittest discover bench/showcase`
(run.sh runs them before a run)."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import report  # noqa: E402


def impl(label, lang, runs=1, error=None, threads=None, **summary):
    d = {"label": label, "lang": lang, "summary": {"runs": runs, **summary}}
    if threads:
        d["threads"] = threads
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
        self.assertEqual(out, ["benchmarks-game: nbody: Rust (Velt main_opt 2.00× slower)"])

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


def game(*rows):
    """A Benchmarks Game record from (label, lang, threads, best wall s) rows."""
    return {"name": "mandelbrot (16000)",
            "impls": [impl(l, g, threads=t, best_wall_s=w, median_wall_s=w, min_cpu_s=w) for l, g, t, w in rows]}


class Pairing(unittest.TestCase):
    """Single-threaded rows are compared with Velt's single-threaded program, multi-threaded rows
    with Velt's multi-threaded one, never across."""

    def test_each_kind_against_its_velt(self):
        rec = game(("Rust mandelbrot", "rust", "mt", 0.5), ("Rust mandelbrot_st", "rust", "st", 3.0),
                   ("Velt main", "velt", "st", 19.5), ("Velt main_mt", "velt", "mt", 2.0),
                   ("Node main", "node", "mt", 1.0), ("Node main_st", "node", "st", 12.0))
        out = report.losses(results("benchmarks-game", rec))
        self.assertEqual(out, [
            "benchmarks-game: mandelbrot (16000) [multi-threaded]: Rust mandelbrot (Velt main_mt 4.00× slower)",
            "benchmarks-game: mandelbrot (16000) [single-threaded]: Rust mandelbrot_st (Velt main 6.50× slower)",
        ])

    def test_multi_threaded_competitor_never_beats_single_threaded_velt(self):
        # Velt's single-threaded program beats every single-threaded competitor and its
        # multi-threaded one every multi-threaded competitor: nothing is lost, although Rust on
        # all cores is 39 times faster than Velt on one.
        rec = game(("Rust mandelbrot", "rust", "mt", 0.5), ("Rust mandelbrot_st", "rust", "st", 21.0),
                   ("Velt main", "velt", "st", 19.5), ("Velt main_mt", "velt", "mt", 0.4))
        self.assertEqual(report.losses(results("benchmarks-game", rec)), [])

    def test_no_velt_of_that_kind(self):
        rec = game(("Rust fasta", "rust", "mt", 0.6), ("Rust fasta_st", "rust", "st", 1.8),
                   ("Velt main", "velt", "st", 1.5))
        out = report.losses(results("benchmarks-game", rec))
        self.assertEqual(out, ["benchmarks-game: mandelbrot (16000) [multi-threaded]: Rust fasta: "
                               "no Velt multi-threaded version"])

    def test_no_velt_single_threaded_version(self):
        rec = {"name": "/plaintext", "kind": "http",
               "impls": [impl("Velt velt:http", "velt", threads="mt", best_rps=100.0),
                         impl("Node node:http (1 process)", "node", threads="st", best_rps=50.0)]}
        out = report.losses(results("http", rec))
        self.assertEqual(out, ["http: /plaintext: req/s [single-threaded]: Node node:http (1 process): "
                               "no Velt single-threaded version"])

    def test_velt_failed_of_one_kind(self):
        rec = {"name": "fasta", "impls": [impl("Velt main", "velt", threads="st", best_wall_s=1.0),
                                          impl("Velt main_mt", "velt", 0, error="exit 1", threads="mt"),
                                          impl("Rust fasta", "rust", threads="mt", best_wall_s=0.5)]}
        out = report.losses(results("benchmarks-game", rec))
        self.assertEqual(out, ["benchmarks-game: fasta [multi-threaded]: Velt: n/a (exit 1)"])

    def test_websocket_single_process_rows_against_velt_one_thread(self):
        def chat(bun_p99):
            return {"name": "chat", "kind": "websocket",
                    "impls": [impl("Velt (6 threads)", "velt", threads="mt", p99_ms=4.0),
                              impl("Velt (1 thread)", "velt", threads="st", p99_ms=10.0),
                              impl("Bun (single process; rooms need shared state)", "bun", threads="st",
                                   p99_ms=bun_p99)]}
        # Bun's one process is slower than Velt on 6 threads but faster than Velt on one: a loss
        # against the like-for-like row, not a win of the 6-thread one.
        out = report.losses(results("websocket", chat(5.0)))
        self.assertEqual(out, ["websocket: chat: p99 [single-threaded]: Bun (single process; rooms need shared "
                               "state) (Velt (1 thread) 2.00× slower)"])
        self.assertEqual(report.losses(results("websocket", chat(20.0))), [])

    def test_vs_velt_cell_pairs_by_kind(self):
        rec = game(("Rust mandelbrot", "rust", "mt", 0.5), ("Velt main", "velt", "st", 19.5),
                   ("Velt main_mt", "velt", "mt", 2.0), ("Bun main_st", "bun", "st", 9.75))
        lines = []
        report.process_table(rec, lines, [])
        cells = {line.split(" | ")[1]: line.rstrip(" |").split(" | ")[-1] for line in lines}
        self.assertEqual(cells["Rust mandelbrot"], "Velt main_mt 4.00× slower")
        self.assertEqual(cells["Bun main_st"], "Velt main 2.00× slower")
        self.assertEqual(cells["Velt main"], "same")
        self.assertEqual(cells["Velt main_mt"], "same")

    def test_vs_velt_cell_without_counterpart(self):
        rec = {"name": "fib", "impls": [impl("Velt", "velt", threads="st", best_wall_s=1.0, median_wall_s=1.0, min_cpu_s=1.0),
                                        impl("Rust rayon", "rust", threads="mt", best_wall_s=0.5, median_wall_s=0.5,
                                             min_cpu_s=0.5)]}
        lines = []
        report.process_table(rec, lines, [])
        self.assertTrue(lines[1].endswith("| no Velt multi-threaded version |"), lines[1])


class Tables(unittest.TestCase):
    def test_failed_server_category_is_a_note(self):
        # http_benchmarks returns this record when oha is missing: the table says so.
        for cat in ("http", "websocket"):
            md = report.category_md(cat, [{"name": "techempower", "error": "oha not found", "impls": []}])
            self.assertIn("- techempower: oha not found", md)


class Memory(unittest.TestCase):
    def rows(self, rec, cat="benchmarks-game"):
        md = report.memory_md({"categories": {cat: [rec]}})
        return {line.split(" | ")[2]: line.split(" | ")[3:] for line in md.splitlines()
                if line.startswith(f"| {cat} |")}

    def test_each_kind_its_own_row(self):
        mb = lambda label, lang, t, rss: impl(label, lang, threads=t, peak_rss_mb=rss)
        rec = {"name": "binary-trees (21)", "impls": [
            mb("Rust binary-trees", "rust", "mt", 442.0), mb("Rust binary-trees_box", "rust", "st", 162.0),
            mb("Rust binary-trees_st", "rust", "st", 130.0), mb("Velt main", "velt", "st", 99.5),
            mb("Velt main_mt", "velt", "mt", 161.0), mb("Node main", "node", "mt", 1365.0)]}
        rows = self.rows(rec)
        # Rust's `_st` program, not the first single-threaded one by name (`_box`); no Node cell
        # in the single-threaded row: its only program uses worker_threads.
        self.assertEqual(rows["1"], ["99.5", "130", "", " |"])
        self.assertEqual(rows["many"], ["161", "442", "1,365", " |"])

    def test_server_on_all_cores_not_beside_one_thread(self):
        srv = lambda label, lang, t, rss: impl(label, lang, threads=t, server_peak_rss_mb=rss)
        rec = {"name": "/plaintext", "kind": "http", "impls": [
            srv("Velt (1 thread)", "velt", "st", 4.9), srv("Rust axum", "rust", "mt", 4.3),
            srv("Velt", "velt", "mt", 5.8)]}
        rows = self.rows(rec, "http")
        self.assertEqual(rows["1"][:2], ["4.9", ""])
        self.assertEqual(rows["many"][:2], ["5.8", "4.3"])


class Definitions(unittest.TestCase):
    def test_every_benchmarks_game_implementation_is_classified(self):
        import suites
        here = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "benchmarks-game")
        bins = os.listdir(os.path.join(here, "rust", "src", "bin"))
        programs = [d for d in os.listdir(here) if os.path.isfile(os.path.join(here, d, "bench.conf"))]
        self.assertTrue(programs)
        for p in programs:
            table = suites.GAME_THREADS.get(p, {})
            files = [("velt", f[:-4]) for f in os.listdir(os.path.join(here, p)) if f.endswith(".vlt")]
            files += [("js", f[:-3]) for f in os.listdir(os.path.join(here, p)) if f.endswith(".js")]
            files += [("rust", f[:-3]) for f in bins if f == f"{p}.rs" or f.startswith(f"{p}_")]
            for f in files:
                self.assertIn(table.get(f, "missing"), (suites.ST, suites.MT), f"{p}: {f}")

    def test_known_kinds(self):
        import suites
        t = suites.GAME_THREADS
        self.assertEqual(t["fasta"][("rust", "fasta")], suites.MT)
        self.assertEqual(t["fasta"][("rust", "fasta_st")], suites.ST)
        self.assertEqual(t["fasta"][("js", "main")], suites.MT)
        self.assertEqual(t["reverse-complement"][("js", "main")], suites.ST)
        self.assertEqual(t["n-body"][("rust", "n-body")], suites.ST)
        self.assertEqual(t["binary-trees"][("rust", "binary-trees_box")], suites.ST)
        self.assertEqual(t["mandelbrot"][("velt", "main_mt")], suites.MT)
        self.assertEqual(t["mandelbrot"][("velt", "main_opt")], suites.ST)

    def test_kinds_match_the_sources(self):
        # A multi-threaded file uses threads (Rust: rayon or std::thread; JS: worker_threads;
        # Velt: spawn), a single-threaded one does not, except Rust `_st`, which is the
        # multi-threaded program on a rayon pool of one thread.
        import re
        import suites
        here = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "benchmarks-game")
        marks = {"rust": r"use rayon|extern crate rayon|thread::(spawn|scope)|std::thread",
                 "js": r"worker_threads", "velt": r"\bspawn\("}
        for program, table in suites.GAME_THREADS.items():
            for (lang, stem), k in table.items():
                if lang == "rust":
                    path = os.path.join(here, "rust", "src", "bin", f"{stem}.rs")
                else:
                    path = os.path.join(here, program, f"{stem}.{'vlt' if lang == 'velt' else 'js'}")
                if not os.path.isfile(path):
                    continue
                src = open(path, encoding="utf-8").read()
                threaded = re.search(marks[lang], src) is not None
                if lang == "rust" and stem.endswith("_st") and threaded:
                    self.assertIn("num_threads(1)", src, path)
                    continue
                self.assertEqual(threaded, k == suites.MT, f"{path}: listed as {k}")

    def test_threads_is_required(self):
        import suites
        with self.assertRaises(TypeError):
            suites.impl("Velt", "velt", ["x"])
        with self.assertRaises(ValueError):
            suites.impl("Velt", "velt", ["x"], threads="2")


if __name__ == "__main__":
    unittest.main()

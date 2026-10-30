"""Tests of the measurement: the measure helper's peak RSS is the program's own (Linux, with a C
compiler), and the wait for a quiet machine before timing. `python3 -m unittest discover
bench/showcase` (run.sh runs them before a run)."""

import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import engine  # noqa: E402
import showcase  # noqa: E402


@unittest.skipUnless(sys.platform.startswith("linux") and (shutil.which("cc") or shutil.which("clang")),
                     "the measure helper is Linux-only and needs a C compiler")
class Measure(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.mkdtemp(prefix="showcase-measure-")
        cls.exe = engine.build_measure(cls.dir)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.dir, ignore_errors=True)

    def setUp(self):
        self.saved, engine.MEASURE = engine.MEASURE, self.exe

    def tearDown(self):
        engine.MEASURE = self.saved

    def rss(self, *cmd):
        return engine.run_proc(list(cmd))[0]["rss_mb"]

    def test_different_programs_report_different_rss(self):
        # The harness holds 300 MB: the old wait4 measurement reported at least that for every
        # program it started afterwards.
        ballast = b"x" * (300 << 20)
        small = self.rss("true")
        big = self.rss(sys.executable, "-c", "b = b'x' * (200 << 20)")
        del ballast
        self.assertLess(small, 16)
        self.assertGreater(big, 200)
        self.assertLess(big, 300)

    def test_check_measure_passes(self):
        self.assertIsNone(engine.check_measure())

    def test_times_and_output_pass_through(self):
        m, out = engine.run_proc(["sh", "-c", "echo hi; i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done"])
        self.assertEqual(out, b"hi\n")
        self.assertGreater(m["wall_s"], 0)
        self.assertGreater(m["cpu_s"], 0)

    def test_exit_code_is_the_programs(self):
        with self.assertRaisesRegex(engine.RunError, "exit 3"):
            engine.run_proc(["sh", "-c", "echo oops >&2; exit 3"])

    def test_timeout_kills_the_program(self):
        with self.assertRaises(engine.RunError):
            engine.run_proc(["sleep", "30"], timeout=0.5)

    def test_without_helper_rss_is_unknown(self):
        engine.MEASURE = None
        m, _ = engine.run_proc(["true"])
        self.assertIsNone(m["rss_mb"])


class Settle(unittest.TestCase):
    def fake(self, loads):
        t = [0.0]
        it = iter(loads)
        last = [None]

        def load():
            last[0] = next(it, last[0])
            return last[0]

        def sleep(s):
            t[0] += s
        return dict(now=lambda: t[0], sleep=sleep, load=load, poll=5.0)

    def test_waits_until_quiet(self):
        rec = showcase.settle(2.0, 900, False, **self.fake([9.0, 6.0, 3.0, 1.5]))
        self.assertEqual(rec["load_before"], 9.0)
        self.assertEqual(rec["load_after"], 1.5)
        self.assertEqual(rec["waited_s"], 15.0)
        self.assertFalse(rec["timed_out"])

    def test_quiet_already(self):
        rec = showcase.settle(2.0, 900, False, **self.fake([0.5]))
        self.assertEqual(rec["waited_s"], 0.0)

    def test_timeout_stops_the_run(self):
        with self.assertRaises(SystemExit) as e:
            showcase.settle(2.0, 20, False, **self.fake([5.0]))
        self.assertIn("still 5.00 after 20 s", str(e.exception))

    def test_timeout_with_force_goes_on(self):
        rec = showcase.settle(2.0, 20, True, **self.fake([5.0]))
        self.assertTrue(rec["timed_out"])
        self.assertEqual(rec["waited_s"], 20.0)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Tests of velt-ext.py in scratch repositories: `python3 -I scripts/velt-ext-test.py`."""

import os
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "velt-ext.py")


class Repo(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = self.tmp.name
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "t")
        self.git("config", "user.email", "t@example.com")
        self.write("a.vlt", "import './b.vlt';\n")
        self.write("b.vlt", "b\n")
        self.write("gone.vlt", "gone\n")
        self.write("keep.txt", 'x = "vlt"; // velt-ext: keep\ny = "vlt";\n')
        self.commit("base")

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.dir, check=True, capture_output=True, text=True
        ).stdout.strip()

    def write(self, name, body):
        with open(os.path.join(self.dir, name), "w") as f:
            f.write(body)

    def read(self, name):
        with open(os.path.join(self.dir, name)) as f:
            return f.read()

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def run_script(self, *args):
        return subprocess.run(
            [sys.executable, "-I", SCRIPT, *args], cwd=self.dir, capture_output=True, text=True
        )

    def move(self, *args):
        return self.run_script("branch", "--onto", "new", *args)

    def rename_main(self):
        """`new`: main with the rename, and what the branch under test is rebased on."""
        here = self.git("symbolic-ref", "--short", "HEAD")
        self.git("checkout", "-q", "-b", "new", "main")
        self.assertEqual(self.run_script("commit").returncode, 0)
        self.git("checkout", "-q", here)

    def files(self, rev="HEAD"):
        return sorted(self.git("ls-tree", "-r", "--name-only", rev).split())

    def changed(self):
        return sorted(self.git("diff", "--name-only", "new", "HEAD").split())

    def feature(self, name="f"):
        self.git("checkout", "-q", "-b", name, "main")

    def test_commit_renames_rewrites_and_keeps_marked_lines(self):
        self.rename_main()
        self.git("checkout", "-q", "new")
        self.assertEqual(self.files(), ["a.velt", "b.velt", "gone.velt", "keep.txt"])
        self.assertEqual(self.read("a.velt"), "import './b.velt';\n")
        self.assertEqual(self.read("keep.txt"), 'x = "vlt"; // velt-ext: keep\ny = "velt";\n')
        self.assertIn("already", self.run_script("commit").stdout)

    def test_a_branch_moves_with_edits_and_new_files(self):
        self.feature()
        self.write("b.vlt", "edited\n")
        self.write("added.vlt", "new\n")
        self.commit("change")
        self.rename_main()
        p = self.move()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(self.changed(), ["added.velt", "b.velt"])
        self.assertEqual(self.read("b.velt"), "edited\n")
        self.assertEqual(self.run_script("verify", "--onto", "new").returncode, 0)

    def test_an_untracked_file_is_not_overwritten(self):
        self.feature()
        self.write("c.vlt", "tracked\n")
        self.commit("change")
        self.rename_main()
        self.write("c.velt", "someone's work\n")
        before = self.git("rev-parse", "HEAD")
        p = self.move()
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("c.velt", p.stderr)
        self.assertEqual(self.read("c.velt"), "someone's work\n")
        self.assertEqual(self.git("rev-parse", "HEAD"), before)

    def stack(self):
        self.feature("low")
        self.write("low.vlt", "low\n")
        self.commit("low")
        self.git("checkout", "-q", "-b", "top")
        self.write("top.vlt", "top\n")
        self.commit("top")

    def test_a_stack_moves_together(self):
        self.stack()
        self.rename_main()
        p = self.move("--stack")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(self.changed(), ["low.velt", "top.velt"])
        self.assertEqual(self.git("rev-parse", "low"), self.git("rev-parse", "HEAD~1"))
        self.assertEqual(self.git("rev-parse", "HEAD~2"), self.git("rev-parse", "new"))

    def test_the_way_back_from_a_conflict_restores_every_branch(self):
        self.stack()
        low, top = self.git("rev-parse", "low"), self.git("rev-parse", "top")
        self.git("checkout", "-q", "main")
        self.write("low.vlt", "conflicting\n")
        self.commit("main changes low")
        self.git("checkout", "-q", "top")
        self.rename_main()
        p = self.move("--stack")
        self.assertNotEqual(p.returncode, 0)
        back = p.stderr.strip().splitlines()[-1].strip()
        subprocess.run(back, shell=True, cwd=self.dir, check=True, capture_output=True)
        self.assertEqual(self.git("rev-parse", "low"), low)
        self.assertEqual(self.git("rev-parse", "top"), top)

    def test_stack_names_a_lower_branch_that_has_diverged(self):
        self.stack()
        self.git("checkout", "-q", "-b", "above", "top")
        self.write("above.vlt", "above\n")
        self.commit("above")
        self.git("checkout", "-q", "top")
        self.git("checkout", "-q", "low")
        self.write("low2.vlt", "more\n")
        self.commit("low moves on")
        low = self.git("rev-parse", "low")
        self.git("checkout", "-q", "top")
        self.rename_main()
        p = self.move("--stack")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("low: forks from this branch", p.stdout)
        self.assertNotIn("above", p.stdout)
        self.assertEqual(self.git("rev-parse", "low"), low)

    def test_stack_leaves_branches_outside_the_range_alone(self):
        self.git("branch", "other", "main")
        self.stack()
        other = self.git("rev-parse", "other")
        self.rename_main()
        self.assertEqual(self.move("--stack").returncode, 0)
        self.assertEqual(self.git("rev-parse", "other"), other)
        self.assertEqual(self.git("rev-parse", "main"), other)

    def test_a_second_run_changes_nothing_and_keeps_the_backup(self):
        self.feature()
        self.write("c.vlt", "c\n")
        self.commit("change")
        old = self.git("rev-parse", "HEAD")
        self.rename_main()
        self.assertEqual(self.move().returncode, 0)
        moved = self.git("rev-parse", "HEAD")
        p = self.move()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("already", p.stdout)
        self.assertEqual(self.git("rev-parse", "HEAD"), moved)
        self.assertEqual(self.git("rev-parse", "refs/velt-ext/backup/f"), old)

    def test_old_names_after_a_move_do_not_replace_the_backup(self):
        self.feature()
        self.write("c.vlt", "c\n")
        self.commit("change")
        old = self.git("rev-parse", "HEAD")
        self.rename_main()
        self.assertEqual(self.move().returncode, 0)
        self.write("d.vlt", "d\n")
        self.commit("old name again")
        self.assertNotEqual(self.move().returncode, 0)
        self.assertEqual(self.git("rev-parse", "refs/velt-ext/backup/f"), old)

    def test_a_target_without_the_rename_is_refused(self):
        self.feature()
        self.write("c.vlt", "c\n")
        self.commit("change")
        before = self.git("rev-parse", "HEAD")
        p = self.run_script("branch", "--onto", "main")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("no rename yet", p.stderr)
        self.assertEqual(self.git("rev-parse", "HEAD"), before)

    def merged_branch(self):
        """A branch that merged main after main deleted a file, with a change only in the merge."""
        self.feature()
        self.write("f.vlt", "f\n")
        self.commit("f")
        self.git("checkout", "-q", "main")
        self.git("rm", "-q", "gone.vlt")
        self.commit("main deletes gone")
        self.git("checkout", "-q", "f")
        self.git("merge", "-q", "--no-commit", "main")
        self.write("only_in_merge.vlt", "m\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "merge main")

    def test_merges_are_refused_without_squash(self):
        self.merged_branch()
        self.rename_main()
        before = self.git("rev-parse", "HEAD")
        p = self.move()
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("--squash", p.stderr)
        self.assertEqual(self.git("rev-parse", "HEAD"), before)

    def test_squash_keeps_a_merge_only_change_and_no_deleted_file(self):
        self.merged_branch()
        self.rename_main()
        p = self.move("--squash")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(self.changed(), ["f.velt", "only_in_merge.velt"])
        self.assertEqual(self.git("rev-list", "--count", "new..HEAD"), "1")

    def test_a_file_where_the_moved_tree_has_a_folder_is_not_overwritten(self):
        self.feature()
        os.mkdir(os.path.join(self.dir, "newdir.vlt"))
        self.write("newdir.vlt/inner.vlt", "inner\n")
        self.commit("change")
        self.rename_main()
        self.write("newdir.velt", "someone's work\n")
        p = self.move()
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("newdir.velt", p.stderr)
        self.assertEqual(self.read("newdir.velt"), "someone's work\n")

    def lost(self, first, second):
        """Whether verify sees a branch's added line `first` turned into `second`."""
        self.feature()
        self.write("c.vlt", first)
        self.commit("change")
        self.rename_main()
        self.assertEqual(self.move().returncode, 0)
        self.write("c.velt", second)
        self.commit("changed in place")
        return self.run_script("verify", "--onto", "new").returncode != 0

    def test_verify_reads_lines_that_look_like_headers(self):
        self.assertTrue(self.lost("++i;\n", "++j;\n"))

    def test_verify_notices_a_changed_binary_file(self):
        self.assertTrue(self.lost("\0one\n", "\0two\n"))

    def test_verify_notices_a_changed_mode(self):
        self.feature()
        self.write("c.vlt", "c\n")
        self.commit("change")
        self.rename_main()
        self.assertEqual(self.move().returncode, 0)
        self.git("update-index", "--chmod=+x", "c.velt")
        self.git("commit", "-q", "-m", "mode")
        self.assertNotEqual(self.run_script("verify", "--onto", "new").returncode, 0)

    def test_verify_checks_each_branch_of_a_stack(self):
        self.stack()
        self.rename_main()
        self.assertEqual(self.move("--stack").returncode, 0)
        self.assertEqual(self.run_script("verify", "--onto", "new", "top", "low").returncode, 0)
        self.git("checkout", "-q", "low")
        self.write("low.velt", "changed\n")
        self.commit("changed in place")
        self.git("checkout", "-q", "top")
        p = self.run_script("verify", "--onto", "new", "top", "low")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("refs/velt-ext/backup/low", p.stderr)

    def test_after_going_back_a_stack_can_move_again(self):
        self.stack()
        self.git("checkout", "-q", "main")
        self.write("low.vlt", "conflicting\n")
        self.commit("main changes low")
        self.git("checkout", "-q", "top")
        self.rename_main()
        p = self.move("--stack")
        self.assertNotEqual(p.returncode, 0)
        back = p.stderr.strip().splitlines()[-1].strip()
        subprocess.run(back, shell=True, cwd=self.dir, check=True, capture_output=True)
        self.assertEqual(self.git("for-each-ref", "refs/velt-ext"), "")
        p = self.move("--stack")
        self.assertIn("real conflict", p.stderr, "stopped by the conflict, not by a backup")

    def test_verify_notices_a_lost_change(self):
        self.feature()
        self.write("c.vlt", "c\n")
        self.commit("change")
        self.rename_main()
        self.assertEqual(self.move().returncode, 0)
        self.git("rm", "-q", "c.velt")
        self.write("d.velt", "something else\n")
        self.commit("lost")
        p = self.run_script("verify", "--onto", "new")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("differ", p.stderr)


if __name__ == "__main__":
    unittest.main()

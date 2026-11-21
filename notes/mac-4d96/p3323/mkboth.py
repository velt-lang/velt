"""Scratch std: parked-away's (the #3323 prototype) with #3360's fixed settle.velt from the
settle-race worktree: python3 mkboth.py <dir>  -> <dir>/std-both"""
import shutil
import sys

B = "/Users/andii/dev/velt/branches"
dst = sys.argv[1] + "/std-both"
shutil.rmtree(dst, ignore_errors=True)
shutil.copytree(B + "/parked-away/std", dst)
shutil.copyfile(B + "/settle-race/std/lang/promise/settle.velt", dst + "/lang/promise/settle.velt")
print(dst)

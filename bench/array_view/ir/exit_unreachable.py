# rows_index (bench/array_view/run.sh), built by the compiler of the commit adding this file:
# the copy's `i < rows[k].length` test of `rows[k][i]` (bb137 of `velt_main`) goes to an
# out-of-bounds error (bb34, a noreturn call) instead of on into the loop's original (bb143).
# Not a valid program: it shows what the count would be without that way on into the original.
import sys

src = sys.stdin.read()
m = src.find('define dso_local i32 @"velt_main"')
if m >= 0:
    body = src[m:]
    old = "  br i1 %t877, label %bb138, label %bb143\n"
    assert body.count(old) == 1, "IR changed: find the copy's index test again"
    src = src[:m] + body.replace(old, "  br i1 %t877, label %bb138, label %bb34\n")
sys.stdout.write(src)

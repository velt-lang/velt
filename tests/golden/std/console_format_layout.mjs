// The Node twin of tests/golden/std/console_format_layout.velt.
class Point {
  constructor(x, y, label, extra) { this.x = x; this.y = y; this.label = label; this.extra = extra; }
}
const p = new Point(1, 2, "a label that is fairly long", "and another string to break it");
console.log("%s", p);
const o = { alpha: "aaaaaaaaaaaaaaaaaaaa", beta: "bbbbbbbbbbbbbbbbbbbb", gamma: "cccccccccccccccc", n: [1, 2, 3] };
console.log("[%s]", o);
const arr = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30];
console.log("%s|", arr);
const nested = { a: { b: 1 }, list: [[1], [2]], s: "some string that is long enough to wrap the line for sure" };
console.log("x %s y", nested);

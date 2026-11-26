// The Node twin of tests/golden/std/regexp_last_index_any.velt; its .out is what Node 22
// prints.

function from(label, v) {
  const re = /b/g;
  const a = re;
  a.lastIndex = v;
  const m = re.exec("abcabc");
  console.log(label, m === null ? "no match" : m.index, re.lastIndex);
}

const sticky = /b/y;
const s = sticky;
s.lastIndex = "1";
console.log("sticky", sticky.test("abc"), sticky.lastIndex);

from("'3'", "3");
from("' 4 '", " 4 ");
from("'abc'", "abc");
from("null", null);
from("true", true);
from("-5", -5);
from("'1e1'", "1e1");

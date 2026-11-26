// The Node twin of tests/golden/std/regex_subclass_exec_throws.velt (same output).
let calls = 0;

// `exec` refuses a subject of more than 5 characters.
class Strict extends RegExp {
  exec(s) {
    calls++;
    if (s.length > 5) {
      throw new Error("subject too long");
    }
    return super.exec(s);
  }
}

// `exec` throws on its second call.
class Second extends RegExp {
  n = 0;
  exec(s) {
    calls++;
    if (++this.n == 2) {
      throw new RangeError("second call");
    }
    return super.exec(s);
  }
}

function attempt(label, re, f) {
  let result = "";
  try {
    result = JSON.stringify(f(re));
  } catch (e) {
    const what = e instanceof RangeError ? "RangeError: " + e.message : e instanceof Error ? "Error: " + e.message : "?";
    result = "threw " + what;
  }
  console.log(label, result, "calls", calls, "lastIndex", re.lastIndex);
  calls = 0;
}

const long = "abcabcabc";
for (const flags of ["", "g", "y"]) {
  console.log("flags", JSON.stringify(flags));
  attempt("test short", new Strict("b", flags), (re) => re.test("abc"));
  attempt("test", new Strict("b", flags), (re) => re.test(long));
  attempt("match", new Strict("b", flags), (re) => long.match(re));
  attempt("search", new Strict("b", flags), (re) => long.search(re));
  attempt("replace", new Strict("b", flags), (re) => long.replace(re, "-"));
  attempt("replace calling", new Strict("b", flags), (re) => long.replace(re, (m) => m + m));
  attempt("split", new Strict("b", flags), (re) => long.split(re));
  if (flags == "g") {
    attempt("matchAll", new Strict("b", flags), (re) => [...long.matchAll(re)].length);
    attempt("replaceAll", new Strict("b", flags), (re) => long.replaceAll(re, "-"));
  }
}

// After a throw from the second call: `lastIndex` as JavaScript leaves it, and what an ordinary
// `exec` then finds (the regex is no sticky copy any more).
console.log("the second call throws");
function after(label, re, f) {
  attempt(label, re, f);
  const m = re.exec(long);
  console.log("  then exec:", m ? m[0] + "@" + m.index : null, "lastIndex", re.lastIndex);
  calls = 0;
}
let second = new Second("b", "g");
second.lastIndex = 3;
after("split", second, (re) => long.split(re));
second = new Second("b", "g");
second.lastIndex = 3;
after("matchAll", second, (re) => [...long.matchAll(re)].length);
second = new Second("b", "g");
second.lastIndex = 3;
after("match", second, (re) => long.match(re));
second = new Second("b", "g");
second.lastIndex = 3;
after("replace", second, (re) => long.replace(re, "-"));
second = new Second("b", "g");
second.lastIndex = 3;
after("search", second, (re) => long.search(re) + long.search(re));
second = new Second("b");
after("split, no flag", second, (re) => long.split(re));

// A replacer that throws, with an `exec` that does not.
console.log("a replacer throws");
attempt("replace", new Strict("b", "g"), (re) => "abcab".replace(re, (m) => {
  if (m.length > 0) {
    throw new Error("replacer " + m);
  }
  return m;
}));

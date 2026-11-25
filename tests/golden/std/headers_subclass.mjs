// The Node twin of tests/golden/std/headers_subclass.velt; its .out is what Node 22 prints.
class Loud extends Headers {
  append(name, value) {
    console.log("append", name, value);
    super.append(name, value);
  }
  get(name) {
    console.log("get", name);
    return "overridden";
  }
  getSetCookie() {
    console.log("getSetCookie");
    return [];
  }
}

function main() {
  const h = new Loud([
    ["b", "2"],
    ["Set-Cookie", "x=1"],
    ["a", "1"],
    ["b", "3"],
    ["set-cookie", "y=2"],
  ]);
  console.log("made from pairs");
  const r = new Loud({ x: "1", y: "2" });
  console.log("made from a record");
  h.forEach((value, name) => console.log("each", name, value));
  for (const [name, value] of h) {
    console.log("of", name, value);
  }
  console.log([...h.entries()].join("|"));
  console.log([...h.keys()].join("|"));
  console.log([...h.values()].join("|"));
  console.log([...r].join("|"));
  console.log(h.get("b"), h.getSetCookie().length);
  h.append("c", "4");
  console.log([...h.keys()].join("|"));
}

main();

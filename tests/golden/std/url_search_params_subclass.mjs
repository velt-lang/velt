// The Node twin of tests/golden/std/url_search_params_subclass.velt; its .out is what Node 22 prints.
class Loud extends URLSearchParams {
  append(name, value) {
    console.log("append", name, value);
    super.append(name, value);
  }
  entries() {
    console.log("entries");
    return [["z", "26"]];
  }
}

class Quiet extends URLSearchParams {}

function main() {
  const p = new Loud("b=2&a=1&b=3&flag");
  console.log("made", p.size, p.toString());
  p.set("c", "4");
  console.log("set new", p.toString());
  p.set("b", "9");
  console.log("set old", p.toString());
  p.sort();
  console.log("sorted", p.toString());
  p.append("d", "5");
  console.log(p.toString());
  console.log(p.entries().join("|"));
  // `console.log` names a subclass by its class, as Node does; `[Object]` past the depth limit.
  console.log(p);
  console.log(new Quiet(""), new Quiet(`a=${"x".repeat(80)}&b=2`));
  console.log({ a: { b: { c: new Quiet("a=1") } } });
}

main();

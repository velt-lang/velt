// Node twin of record_view_function.velt: the casts are no-ops in TypeScript.
function hello() {
  return 1;
}

function makeThis(n) {
  return function () {
    return n;
  };
}

function counter(start) {
  let n = start;
  return () => {
    n++;
    return n;
  };
}

const f = hello;
const r = f;
const r2 = f;
console.log(`declared: keys ${Object.keys(r).length} same ${r === f} twice ${r === r2}`);
const g = () => 2;
const gr = g;
console.log(`closure: keys ${Object.keys(gr).length} same ${gr === g}`);
g.tag = "x";
console.log(`written through the function: ${gr["tag"]} keys ${Object.keys(gr).join(",")}`);
gr["k"] = 5;
console.log(`written through the view: ${g.k} ${"k" in gr}`);
console.log(`still callable: ${g()}`);
const h1 = hello;
const h2 = hello;
console.log(`two boxings: ${h1 === h2}`);
const c = counter(3);
const c1 = c;
const c2 = c;
console.log(`closure with captures: ${c1 === c2} ${c1 === c}`);
const t1 = makeThis(1);
const t2 = makeThis(2);
console.log(`this functions: ${t1 === t2} ${t2 === t2} ${t1 === t1}`);
const xs = [1, 2, 3];
const mapFn = Reflect.get(xs, "map");
const popFn = Reflect.get(xs, "pop");
console.log(`array methods: ${mapFn === popFn} ${mapFn === mapFn}`);
const ga = (x) => x;
const gb = (x) => [x];
console.log(`generic closures: ${ga === gb}`);

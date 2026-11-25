// Node twin of record_view_function_plain.velt: the casts are no-ops in TypeScript.
function hello() {
  return 1;
}

const a = hello;
const b = hello;
const ra = a;
const rb = b;
console.log(`keys ${Object.keys(ra).length} ${Object.keys(rb).length}`);
console.log(`same ${ra === a} ${rb === b} ${ra === rb}`);
const k = () => 7;
console.log(`closure ${k === k} ${k()}`);
const ga = (x) => x;
const gb = (x) => [x];
console.log(`generic closures: ${ga === gb}`);

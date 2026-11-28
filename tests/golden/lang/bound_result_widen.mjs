// Node twin of bound_result_widen.velt: the same program with its types removed.
class Square {
  area() {
    return 4;
  }
}

function asShape(v) {
  return () => v;
}

function shapeArea(v) {
  const s = v;
  return s.area();
}

function counter(n) {
  const f = (k) => n;
  return f;
}

function flag(b) {
  const f = () => b;
  return f;
}

function next(n) {
  const x = n;
  return x + 1;
}

const named = { area: () => 7, name: () => "n" };
const sq = new Square();
console.log(asShape(sq)().area(), asShape(named)().area(), shapeArea(named));
console.log(asShape(sq)() === sq);
console.log(counter(3)(0), counter(4)(1));
console.log(flag(true)(), flag(false)());
console.log(next(2));

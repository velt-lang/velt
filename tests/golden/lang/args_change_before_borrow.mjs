// The Node twin of args_change_before_borrow.velt.
class Holder {
  vs = [1, 2];
}

function grow(a) {
  for (let i = 0; i < 100; i++) {
    a.push(i);
  }
  return a.length;
}

function reset(h) {
  h.vs = [7, 8, 9];
  return h.vs.length;
}

function show(n, xs) {
  return `${n} ${xs.length} ${xs[0]} ${xs[xs.length - 1]}`;
}

function main() {
  const xs = [1, 2, 3];
  console.log(xs.shift(), xs);
  const ys = [5];
  console.log(show(grow(ys), ys));
  const h = new Holder();
  console.log(show(reset(h), h.vs));
  const zs = [4, 5, 6];
  console.log(zs.pop(), zs.pop(), zs);
}

main();

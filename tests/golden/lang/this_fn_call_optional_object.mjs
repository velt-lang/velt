// Node twin of this_fn_call_optional_object.velt: the same program with its types removed.
const m = function (x) {
  return `${this.name}:${x}`;
};
const k = "m";
let evaluated = 0;

function arg(v) {
  evaluated++;
  return v;
}

function present() {
  const o = { name: "o", m: m };
  console.log(o?.m?.(1));
  console.log(o?.[k]?.(2));
  console.log(o?.[k](3));
  console.log(o?.["m"]?.(arg(4)));
  console.log(o?.z?.(arg(5)));
  console.log(`evaluated ${evaluated}`);
}

function nullish() {
  const none = null;
  const missing = undefined;
  console.log(none?.m?.(arg(6)));
  console.log(missing?.[k]?.(arg(7)));
  console.log(none?.[k](arg(8)));
  console.log(`evaluated ${evaluated}`);
}

let reads = 0;
function source(o) {
  reads++;
  return o;
}

function once() {
  const o = { name: "once", m: m };
  console.log(source(o)?.m?.(9));
  console.log(source(null)?.[k]?.(arg(10)));
  console.log(`reads ${reads}, evaluated ${evaluated}`);
}

function arrays() {
  const fs = [
    function () {
      return this.length;
    },
  ];
  console.log(fs?.[0]?.());
  console.log(fs?.[0]());
}

function changed() {
  // `this` is the object read before the argument, which assigns the variable.
  let o = { name: "first", m: m };
  const second = { name: "second", m: m };
  const swap = () => {
    o = second;
    return 11;
  };
  console.log(o?.m?.(swap()));
  console.log(o?.[k]?.(12));
}

function chains() {
  // A `?.` link further down the chain short-circuits the whole call.
  const o = { name: "o", inner: { name: "inner", m: m } };
  const none = null;
  console.log(o?.inner.m?.(13));
  console.log(o?.inner?.[k]?.(14));
  console.log(none?.inner.m?.(arg(15)));
  console.log(none?.inner[k]?.(arg(16)));
  console.log(`evaluated ${evaluated}`);
}

present();
nullish();
once();
arrays();
changed();
chains();

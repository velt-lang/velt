// Node twin of this_fn_call_targets.velt: the same program with its types removed.
const m = function (x) {
  return `${this.name}:${x}`;
};
const plain = (x) => `plain:${x}`;

function log(what, value) {
  console.log(what);
  return value;
}

function arrays() {
  const fs = [m, plain];
  console.log(fs[0](1), fs[1](2));
  try {
    fs[5](log("arg of fs[5]", 3));
  } catch (e) {
    console.log(e.message);
  }
}

function records() {
  const r = {};
  r["k"] = m;
  r["p"] = plain;
  console.log(r["k"](1), r["p"](2));
  const d = { name: "d", k: m };
  const key = "k";
  console.log(d[key](3));
  try {
    d["name"](4);
  } catch (e) {
    console.log(e.message);
  }
}

function proxies() {
  const target = { name: "target", m: m };
  const reads = [];
  const p = new Proxy(target, {
    get(t, k) {
      reads.push(String(k));
      return t[k];
    },
  });
  console.log(p["m"](1));
  console.log(reads.join(","));
  try {
    p["name"](2);
  } catch (e) {
    console.log(e.message);
  }
}

function getters() {
  const g = {
    name: "g",
    get m() {
      console.log("get m");
      return m;
    },
  };
  console.log(g["m"](log("arg", 1)));
}

function inner(o, k) {
  return o[k](1);
}

async function later() {
  await Promise.resolve(0);
  return 2;
}

async function routing() {
  const o = { name: "o", m: m, n: 5 };
  try {
    o["n"](await later());
  } catch (e) {
    console.log("after an await:", e.message);
  }
  try {
    o["m"](await later());
    console.log(o["m"](await later()));
    o["n"]();
  } catch (e) {
    console.log(e.name, e instanceof TypeError);
  }
  console.log(inner(o, "m"));
}

arrays();
records();
proxies();
getters();
routing().then(() => console.log("done"));

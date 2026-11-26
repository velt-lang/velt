class Narrow extends Error {
  get stack() {
    return "narrow";
  }
}

class Narrower extends Narrow {
  get stack() {
    return "narrower";
  }
}

const e = new Narrower("n");
console.log((e.stack ?? "").split("\n")[0], String(e));
console.log((new Narrow("m").stack ?? "").split("\n")[0]);

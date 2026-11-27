// Node twin of proxy_array_computed_holes.velt.
const target = [1, 2, 3];
const p = new Proxy(target, {
  set(obj, prop, v, r) {
    const ok = Reflect.set(obj, prop, v, r);
    console.log("set", String(prop), "has 3:", Reflect.has(obj, "3"), "length", obj.length);
    return ok;
  },
  deleteProperty(obj, prop) {
    const ok = Reflect.deleteProperty(obj, prop);
    console.log("delete", String(prop), "has:", Reflect.has(obj, prop), "length", obj.length);
    return ok;
  },
});
const parts = ["spl", "ice"];
const m = parts.join("");
console.log(p[m].call(p, 0, 0, 7, 8));
const f = p[m];
console.log(f.call(p, 0, 2));
console.log(target.join(","));

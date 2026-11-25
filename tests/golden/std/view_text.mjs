// The Node twin of tests/golden/std/view_text.velt (#3623).
function show(label, v) {
  console.log(label, String(v), `${v}`, v + "", v.toString());
}

const buf = new ArrayBuffer(8);
show("DataView", new DataView(buf));
show("DataView at 4", new DataView(buf, 4, 2));
show("Uint8Array", new Uint8Array([1, 2, 3]));
show("Int8Array", new Int8Array([-1, 2]));
show("Uint8ClampedArray", new Uint8ClampedArray([255, 0]));
show("Int16Array", new Int16Array([-300, 7]));
show("Uint16Array", new Uint16Array([65535]));
show("Int32Array", new Int32Array([-5, 6]));
show("Uint32Array", new Uint32Array([4294967295]));
show("Float32Array", new Float32Array([1.5, -0.25]));
show("Float64Array", new Float64Array([0.1, NaN, -0, Infinity]));
show("empty", new Uint8Array(0));
show("subarray", new Uint8Array([1, 2, 3, 4]).subarray(1, 3));

const views = [new DataView(buf), new Uint8Array([1, 2])];
console.log("array", String(views), `${views}`, views.join(";"));
const xa = views[1];
const xd = views[0];
console.log("any", String(xa), `${xd}`, xa + "");
console.log("concat", "a" + views[1], "b" + views[0]);

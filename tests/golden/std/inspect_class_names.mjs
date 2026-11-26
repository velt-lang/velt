// The Node twin of tests/golden/std/inspect_class_names.velt; its .out is what Node 22 prints.
class Tg extends EventTarget {}
class Ac extends AbortController {}
class H extends Headers {}
class B extends Blob {}
class Rq extends Request {}
class Rs extends Response {}

function main() {
  console.log(new EventTarget(), new Tg());
  console.log(new AbortController(), new Ac());
  console.log([new Tg()], { a: new Ac() });
  console.log({ a: { t: new Tg() } }, [[new Tg()]]);

  console.log(new Headers({ a: "1" }), new H({ a: "1" }));
  console.log(new Blob(["hi"]), new B(["hi"], { type: "text/plain" }));
  console.log([new H({ a: "1" })], { b: new B(["hi"]) });
  console.log({ a: { h: new H({ a: "1" }), b: new B(["hi"]) } });
  console.log(new Request("https://a.b/"));
  console.log(new Rq("https://a.b/"));
  console.log(new Response("hi"));
  console.log(new Rs("hi"));
  console.log([new Rs("hi")]);
}

main();

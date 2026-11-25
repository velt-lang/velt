// The Node twin of tests/golden/std/url_subclass.velt; its .out is what Node 22 prints.
class Ported extends URL {
  get port() {
    console.log("port get");
    return "0";
  }
  set port(v) {
    console.log("port set", v);
  }
}

class Hosted extends URL {
  get host() {
    console.log("host get");
    return "evil.example";
  }
  get href() {
    console.log("href get");
    return "evil:href";
  }
}

class Text extends URL {
  toString() {
    return "Text.toString";
  }
}

class Json extends URL {
  toJSON() {
    return "Json.toJSON";
  }
}

function main() {
  const p = new Ported("https://a.b:8080/p?q=1#f");
  p.host = "c.d:9090";
  console.log(p.toString());
  p.host = "e.f";
  console.log(p.toString());
  p.host = "g.h:443";
  console.log(p.toString());
  p.port = "1";
  console.log(p.toString(), p.port, p.host);

  const h = new Hosted("https://a.b:8080/p?q=1#f");
  console.log(h.toString());
  console.log(h.toJSON());
  console.log(JSON.stringify({ h }));
  console.log(`${h}`);
  console.log(String(h));
  console.log(h.origin);
  console.log(h.href, h.host, h.hostname);

  const t = new Text("https://a.b/");
  console.log(t.toJSON(), t.href, `${t}`, JSON.stringify(t));
  const j = new Json("https://a.b/");
  console.log(j.toString(), j.href, `${j}`, JSON.stringify(j));
}

main();

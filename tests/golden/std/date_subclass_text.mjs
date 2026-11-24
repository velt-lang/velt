// The Node twin of tests/golden/std/date_subclass_text.velt (#3624).
const T = 1705311000000;
const plain = new Date(T);

class DateText extends Date {
  toDateString() {
    return "DATE";
  }
  toTimeString() {
    return "TIME";
  }
}

class LocaleText extends Date {
  toLocaleDateString() {
    return "LDATE";
  }
  toLocaleTimeString() {
    return "LTIME";
  }
}

class Iso extends Date {
  toISOString() {
    return "ISO";
  }
}

class Text extends Date {
  toString() {
    return "TEXT";
  }
}

class Value extends Date {
  valueOf() {
    return 42;
  }
  getTime() {
    return 43;
  }
}

class Json extends Date {
  toJSON() {
    return "JSON";
  }
}

class Utc extends Date {
  toUTCString() {
    return "UTC";
  }
}

const a = new DateText(T);
console.log("toString", a.toString() == plain.toString(), a.toString().includes("DATE"));
console.log("toDateString", a.toDateString(), a.toTimeString());
console.log("template", `${a}` == `${plain}`, String(a) == String(plain), a + "" == plain + "");
const bad = new DateText(NaN);
console.log("invalid", bad.toString(), bad.toDateString());
const b = new LocaleText(T);
console.log("toLocaleString", b.toLocaleString() == plain.toLocaleString(), b.toLocaleString().includes("LDATE"));
console.log("toLocaleDateString", b.toLocaleDateString(), b.toLocaleTimeString());
const c = new Iso(T);
console.log("toJSON", c.toJSON(), new Iso(NaN).toJSON());
console.log("JSON.stringify", JSON.stringify(c), JSON.stringify([c]), JSON.stringify({ c: c }));
const d = new Text(T);
console.log("toString override", `${d}`, String(d), d + "", `${[d]}`);
const e = new Value(T);
console.log("valueOf override", e.toString() == plain.toString(), e.toISOString(), e.toUTCString(), e.toJSON());
const f = new Json(T);
console.log("toJSON override", JSON.stringify(f), JSON.stringify({ f: f }));
const g = new Utc(T);
console.log("toUTCString override", g.toUTCString(), g.toString() == plain.toString(), g.toISOString());

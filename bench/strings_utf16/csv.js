// CSV split/join (same workload as csv.vlt): 100k generated lines with non-ASCII fields,
// `join("\n")`, `split("\n")`, `split(",")` per line, then re-joined with ";" and "\n".
class Rng {
  seed = 42;

  next(n) {
    this.seed = (this.seed * 48271) % 2147483647;
    return this.seed % n;
  }

  pick(xs) {
    return xs[this.next(xs.length)];
  }
}

const NAMES = "José|Zoë|Ægir|Søren|François|Łukasz|Ana|Bob|Chloé|Dvořák";
const CITIES = "東京|北京|서울|Zürich|São Paulo|Kraków|Москва|Αθήνα|Paris|İzmir";
const TAGS = "😀|🎉 party|🚀🚀|ok|👍🏽|❤️|none|🔥 hot|🍕|—";

const rng = new Rng();
const names = NAMES.split("|");
const cities = CITIES.split("|");
const tags = TAGS.split("|");
const lines = [];
for (let i = 0; i < 100000; i++) {
  lines.push(`${i},${rng.pick(names)},${rng.pick(cities)},${rng.pick(tags)},${rng.next(100000)}`);
}
const text = lines.join("\n");
const rows = text.split("\n");
const out = [];
let fields = 0;
let fieldChars = 0;
for (const row of rows) {
  const cols = row.split(",");
  fields += cols.length;
  for (const c of cols) {
    fieldChars += c.length;
  }
  out.push(cols.join(";"));
}
const result = out.join("\n");
console.log(
  `text=${text.length} rows=${rows.length} fields=${fields} chars=${fieldChars} result=${result.length} sample=${out[54321]}`,
);

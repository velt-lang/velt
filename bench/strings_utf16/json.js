// JSON (same workload as json.vlt): `JSON.stringify` and `JSON.parse` of 50k objects whose
// string fields hold accents, CJK and emoji, 3 rounds.
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
const NOTES =
  "héllo wörld 😀|東京で会いましょう 🎉|ship it 🚀🚀|çà et là|👍🏽 looks good|I ❤️ JSON|plain ascii note|🔥🔥🔥|naïve café|emoji at end 🍕";
const TAGS = "😀|dev|日本|ü|🚀|ops|👍🏽|ß";

const rng = new Rng();
const names = NAMES.split("|");
const notes = NOTES.split("|");
const tags = TAGS.split("|");
const items = [];
for (let i = 0; i < 50000; i++) {
  items.push({
    id: i,
    name: rng.pick(names),
    note: `${rng.pick(notes)} #${rng.next(1000)}`,
    tags: [rng.pick(tags), rng.pick(tags)],
  });
}
let text = "";
let chars = 0;
let count = 0;
let current = items;
for (let round = 0; round < 3; round++) {
  text = JSON.stringify(current);
  current = JSON.parse(text);
  for (const it of current) {
    chars += it.name.length + it.note.length + it.tags[0].length + it.tags[1].length;
    count++;
  }
}
console.log(
  `text=${text.length} items=${count} chars=${chars} sample=${JSON.stringify(current[12345])}`,
);
